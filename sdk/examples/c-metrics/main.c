#include <stdio.h>
#include <stdlib.h>

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
    const char* endpoint = getenv("OTEL_EXPORTER_OTLP_METRICS_ENDPOINT");
    if (endpoint == NULL) {
        endpoint = "http://localhost:4318/v1/metrics";
    }

    otel_otlp_metric_exporter_builder_t* exporter_builder =
        otel_otlp_metric_exporter_builder_new();
    otel_otlp_metric_exporter_builder_set_endpoint(exporter_builder, otel_cstr(endpoint));
    otel_otlp_metric_exporter_builder_set_timeout_millis(exporter_builder, 5000);
    otel_otlp_metric_exporter_builder_set_temporality(
        exporter_builder, OTEL_METRIC_TEMPORALITY_CUMULATIVE);
    otel_metric_exporter_t* exporter = NULL;
    if (otel_otlp_metric_exporter_builder_build(exporter_builder, &exporter) != OTEL_STATUS_OK) {
        return 1;
    }
    otel_otlp_metric_exporter_builder_destroy(exporter_builder);

    otel_periodic_metric_reader_builder_t* reader_builder =
        otel_periodic_metric_reader_builder_new();
    otel_periodic_metric_reader_builder_set_interval_millis(reader_builder, 10000);
    if (otel_periodic_metric_reader_builder_set_exporter(reader_builder, exporter) !=
        OTEL_STATUS_OK) {
        return 2;
    }
    otel_periodic_metric_reader_t* reader = NULL;
    if (otel_periodic_metric_reader_builder_build(reader_builder, &reader) != OTEL_STATUS_OK) {
        return 3;
    }
    otel_periodic_metric_reader_builder_destroy(reader_builder);

    otel_sdk_builder_t* sdk_builder = otel_sdk_builder_new();
    otel_sdk_builder_set_service_name(sdk_builder, otel_cstr("c-metrics-example"));
    if (otel_sdk_builder_add_metric_reader(sdk_builder, reader) != OTEL_STATUS_OK) {
        return 4;
    }
    otel_sdk_t* sdk = NULL;
    if (otel_sdk_build(sdk_builder, &sdk) != OTEL_STATUS_OK) {
        return 5;
    }
    otel_sdk_builder_destroy(sdk_builder);
    if (otel_sdk_set_metrics_as_global(sdk) != OTEL_STATUS_OK) {
        return 6;
    }

    otel_meter_provider_t* provider = otel_global_meter_provider();
    otel_meter_t* meter = otel_meter_provider_get_meter(
        provider, otel_cstr("example.metrics"), otel_cstr("1.0.0"),
        otel_string_view_empty());
    otel_instrument_options_t options = OTEL_INSTRUMENT_OPTIONS_INIT;
    options.description = otel_cstr("Example metric");
    options.unit = otel_cstr("1");

    otel_counter_u64_t* counter = NULL;
    otel_gauge_f64_t* gauge = NULL;
    otel_histogram_f64_t* histogram = NULL;
    double boundaries[] = { 1.0, 5.0, 10.0, 25.0 };
    otel_instrument_options_t histogram_options = options;
    histogram_options.boundaries = boundaries;
    histogram_options.boundary_count = sizeof(boundaries) / sizeof(boundaries[0]);

    otel_meter_create_u64_counter(meter, otel_cstr("requests"), &options, &counter);
    otel_meter_create_f64_gauge(meter, otel_cstr("queue.depth"), &options, &gauge);
    otel_meter_create_f64_histogram(
        meter, otel_cstr("request.duration"), &histogram_options, &histogram);

    otel_key_value_t attributes[] = {
        otel_kv_string(otel_cstr("route"), otel_cstr("/example")),
        otel_kv_int64(otel_cstr("status"), 200)
    };
    otel_counter_u64_add(counter, 1, attributes, 2);
    otel_gauge_f64_record(gauge, 3.0, attributes, 2);
    otel_histogram_f64_record(histogram, 7.5, attributes, 2);

    callback_state_t* callback_state = (callback_state_t*)malloc(sizeof(callback_state_t));
    if (callback_state == NULL) {
        return 7;
    }
    callback_state->value = 42;
    otel_observable_gauge_u64_t* observable = NULL;
    otel_meter_create_u64_observable_gauge(
        meter, otel_cstr("workers.ready"), &options, observe_requests, callback_state,
        destroy_callback_state, &observable);

    (void)otel_sdk_metrics_force_flush(sdk, 0);

    otel_observable_gauge_u64_destroy(observable);
    otel_histogram_f64_destroy(histogram);
    otel_gauge_f64_destroy(gauge);
    otel_counter_u64_destroy(counter);
    otel_meter_destroy(meter);
    otel_meter_provider_destroy(provider);

    (void)otel_sdk_metrics_shutdown(sdk, 5000);
    otel_sdk_destroy(sdk);
    puts("metrics exported");
    return 0;
}
