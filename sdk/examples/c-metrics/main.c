// SPDX-License-Identifier: Apache-2.0

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include <opentelemetry_c/api.h>
#include <opentelemetry_c/otlp_metric_exporter.h>
#include <opentelemetry_c/periodic_metric_reader.h>
#include <opentelemetry_c/sdk.h>

typedef struct callback_state_t {
    uint64_t value;
} callback_state_t;

static void observe_requests(otel_observer_u64_t* observer, void* user_data) {
    callback_state_t* state = (callback_state_t*)user_data;
    otel_key_value_t attributes[] = {
        otel_kv_string(otel_cstr("state"), otel_cstr("ready"))
    };
    (void)otel_observer_u64_observe(observer, state->value, attributes, 1);
}

static void destroy_callback_state(void* user_data) {
    free(user_data);
}

int main(void) {
    int result = 1;
    otel_otlp_metric_exporter_builder_t* exporter_builder = NULL;
    otel_metric_exporter_t* exporter = NULL;
    otel_periodic_metric_reader_builder_t* reader_builder = NULL;
    otel_periodic_metric_reader_t* reader = NULL;
    otel_sdk_builder_t* sdk_builder = NULL;
    otel_sdk_t* sdk = NULL;
    otel_meter_provider_t* provider = NULL;
    otel_meter_t* meter = NULL;
    otel_counter_u64_t* counter = NULL;
    otel_gauge_f64_t* gauge = NULL;
    otel_histogram_f64_t* histogram = NULL;
    otel_observable_gauge_u64_t* observable = NULL;
    callback_state_t* callback_state = NULL;
    const char* transport = getenv("OTEL_C_METRICS_TRANSPORT");
    int use_grpc = transport != NULL && strcmp(transport, "grpc") == 0;
    const char* endpoint = getenv("OTEL_EXPORTER_OTLP_METRICS_ENDPOINT");
    if (endpoint == NULL) {
        endpoint = use_grpc ? "http://localhost:4317" :
                              "http://localhost:4318/v1/metrics";
    }

    exporter_builder = otel_otlp_metric_exporter_builder_new();
    if (exporter_builder == NULL ||
        otel_otlp_metric_exporter_builder_set_transport(
            exporter_builder, use_grpc ? OTEL_OTLP_METRIC_TRANSPORT_GRPC :
                                         OTEL_OTLP_METRIC_TRANSPORT_HTTP_PROTOBUF) !=
            OTEL_STATUS_OK ||
        otel_otlp_metric_exporter_builder_set_endpoint(exporter_builder,
                                                       otel_cstr(endpoint)) != OTEL_STATUS_OK ||
        otel_otlp_metric_exporter_builder_set_timeout_millis(exporter_builder, 5000) !=
            OTEL_STATUS_OK ||
        otel_otlp_metric_exporter_builder_set_temporality(
            exporter_builder, OTEL_METRIC_TEMPORALITY_CUMULATIVE) != OTEL_STATUS_OK) {
        goto cleanup;
    }
    if (otel_otlp_metric_exporter_builder_build(exporter_builder, &exporter) != OTEL_STATUS_OK) {
        goto cleanup;
    }
    otel_otlp_metric_exporter_builder_destroy(exporter_builder);
    exporter_builder = NULL;

    reader_builder = otel_periodic_metric_reader_builder_new();
    if (reader_builder == NULL ||
        otel_periodic_metric_reader_builder_set_interval_millis(reader_builder, 10000) !=
            OTEL_STATUS_OK) {
        goto cleanup;
    }
    if (otel_periodic_metric_reader_builder_set_exporter(reader_builder, exporter) !=
        OTEL_STATUS_OK) {
        goto cleanup;
    }
    exporter = NULL;
    if (otel_periodic_metric_reader_builder_build(reader_builder, &reader) != OTEL_STATUS_OK) {
        goto cleanup;
    }
    otel_periodic_metric_reader_builder_destroy(reader_builder);
    reader_builder = NULL;

    sdk_builder = otel_sdk_builder_new();
    if (sdk_builder == NULL ||
        otel_sdk_builder_set_service_name(sdk_builder, otel_cstr("c-metrics-example")) !=
            OTEL_STATUS_OK) {
        goto cleanup;
    }
    if (otel_sdk_builder_add_metric_reader(sdk_builder, reader) != OTEL_STATUS_OK) {
        goto cleanup;
    }
    reader = NULL;
    if (otel_sdk_build(sdk_builder, &sdk) != OTEL_STATUS_OK) {
        goto cleanup;
    }
    otel_sdk_builder_destroy(sdk_builder);
    sdk_builder = NULL;
    if (otel_sdk_set_metrics_as_global(sdk) != OTEL_STATUS_OK) {
        goto cleanup;
    }

    provider = otel_global_meter_provider();
    if (provider == NULL) {
        goto cleanup;
    }
    meter = otel_meter_provider_get_meter(
        provider, otel_cstr("example.metrics"), otel_cstr("1.0.0"),
        otel_string_view_empty());
    if (meter == NULL) {
        goto cleanup;
    }
    otel_instrument_options_t options = OTEL_INSTRUMENT_OPTIONS_INIT;
    options.description = otel_cstr("Example metric");
    options.unit = otel_cstr("1");

    double boundaries[] = { 1.0, 5.0, 10.0, 25.0 };
    otel_instrument_options_t histogram_options = options;
    histogram_options.boundaries = boundaries;
    histogram_options.boundary_count = sizeof(boundaries) / sizeof(boundaries[0]);

    if (otel_meter_create_u64_counter(meter, otel_cstr("requests"), &options, &counter) !=
            OTEL_STATUS_OK ||
        otel_meter_create_f64_gauge(meter, otel_cstr("queue.depth"), &options, &gauge) !=
            OTEL_STATUS_OK ||
        otel_meter_create_f64_histogram(
            meter, otel_cstr("request.duration"), &histogram_options, &histogram) !=
            OTEL_STATUS_OK) {
        goto cleanup;
    }

    otel_key_value_t attributes[] = {
        otel_kv_string(otel_cstr("route"), otel_cstr("/example")),
        otel_kv_int64(otel_cstr("status"), 200)
    };
    if (otel_counter_u64_add(counter, 1, attributes, 2) != OTEL_STATUS_OK ||
        otel_gauge_f64_record(gauge, 3.0, attributes, 2) != OTEL_STATUS_OK ||
        otel_histogram_f64_record(histogram, 7.5, attributes, 2) != OTEL_STATUS_OK) {
        goto cleanup;
    }

    callback_state = (callback_state_t*)malloc(sizeof(callback_state_t));
    if (callback_state == NULL) {
        goto cleanup;
    }
    callback_state->value = 42;
    otel_status_t observable_status = otel_meter_create_u64_observable_gauge(
        meter, otel_cstr("workers.ready"), &options, observe_requests, callback_state,
        destroy_callback_state, &observable);
    /* All API-side validation inputs above are valid, so callback ownership transferred
     * even if the backing SDK rejects creation. */
    callback_state = NULL;
    if (observable_status != OTEL_STATUS_OK ||
        otel_sdk_metrics_force_flush(sdk, 0) != OTEL_STATUS_OK) {
        goto cleanup;
    }

    result = 0;
    puts("metrics exported");

cleanup:
    otel_observable_gauge_u64_destroy(observable);
    otel_histogram_f64_destroy(histogram);
    otel_gauge_f64_destroy(gauge);
    otel_counter_u64_destroy(counter);
    otel_meter_destroy(meter);
    otel_meter_provider_destroy(provider);
    if (sdk != NULL) {
        (void)otel_sdk_metrics_shutdown(sdk, 5000);
    }
    otel_sdk_destroy(sdk);
    otel_sdk_builder_destroy(sdk_builder);
    otel_periodic_metric_reader_destroy(reader);
    otel_periodic_metric_reader_builder_destroy(reader_builder);
    otel_metric_exporter_destroy(exporter);
    otel_otlp_metric_exporter_builder_destroy(exporter_builder);
    free(callback_state);
    return result;
}
