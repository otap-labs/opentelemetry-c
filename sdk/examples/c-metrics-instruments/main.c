// SPDX-License-Identifier: Apache-2.0

#include <opentelemetry_c/manual_metric_reader.h>
#include <opentelemetry_c/metric_exporter.h>
#include <opentelemetry_c/metrics.h>
#include <opentelemetry_c/sdk.h>

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

typedef struct observable_u64_state_t {
    uint64_t value;
} observable_u64_state_t;

typedef struct observable_i64_state_t {
    int64_t value;
} observable_i64_state_t;

typedef struct observable_f64_state_t {
    double value;
} observable_f64_state_t;

typedef struct instruments_state_t {
    int exports;
    int metric_callbacks;
} instruments_state_t;

static void print_status_error(const char* what, otel_status_t status) {
    otel_string_view_t message = otel_last_error_message();
    fprintf(stderr, "%s failed: status=%u", what, (unsigned)status);
    if (message.ptr != NULL && message.len > 0) {
        fprintf(stderr, " (%.*s)", (int)message.len, message.ptr);
    }
    fputc('\n', stderr);
}

static otel_status_t visit_metric(void* user_data, const otel_metric_metadata_t* metadata) {
    instruments_state_t* state = (instruments_state_t*)user_data;
    if (metadata == NULL) {
        return OTEL_STATUS_INVALID_ARGUMENT;
    }
    state->metric_callbacks += 1;
    printf("metric: %.*s kind=%u number=%u\n",
           (int)metadata->name.len,
           metadata->name.ptr ? metadata->name.ptr : "",
           (unsigned)metadata->data_kind,
           (unsigned)metadata->number_kind);
    return OTEL_STATUS_OK;
}

static otel_status_t export_metrics(void* user_data, const otel_metric_batch_t* batch) {
    instruments_state_t* state = (instruments_state_t*)user_data;
    otel_metric_visitor_t visitor;
    memset(&visitor, 0, sizeof(visitor));
    visitor.struct_size = sizeof(visitor);
    visitor.metric = visit_metric;
    state->exports += 1;
    if (batch == NULL) {
        return OTEL_STATUS_INVALID_ARGUMENT;
    }
    return otel_metric_batch_visit(batch, &visitor, user_data);
}

static void destroy_observable_u64_state(void* user_data) {
    free(user_data);
}
static void destroy_observable_i64_state(void* user_data) {
    free(user_data);
}
static void destroy_observable_f64_state(void* user_data) {
    free(user_data);
}

static void observe_u64(otel_observer_u64_t* observer, void* user_data) {
    observable_u64_state_t* state = (observable_u64_state_t*)user_data;
    otel_key_value_t attrs[] = { otel_kv_string(otel_cstr("source"), otel_cstr("callback")) };
    (void)otel_observer_u64_observe(observer, state->value, attrs, 1);
}

static void observe_i64(otel_observer_i64_t* observer, void* user_data) {
    observable_i64_state_t* state = (observable_i64_state_t*)user_data;
    otel_key_value_t attrs[] = { otel_kv_string(otel_cstr("source"), otel_cstr("callback")) };
    (void)otel_observer_i64_observe(observer, state->value, attrs, 1);
}

static void observe_f64(otel_observer_f64_t* observer, void* user_data) {
    observable_f64_state_t* state = (observable_f64_state_t*)user_data;
    otel_key_value_t attrs[] = { otel_kv_string(otel_cstr("source"), otel_cstr("callback")) };
    (void)otel_observer_f64_observe(observer, state->value, attrs, 1);
}

int main(void) {
    int result = 1;
    instruments_state_t exporter_state;
    otel_custom_metric_exporter_callbacks_t callbacks;
    otel_metric_exporter_t* exporter = NULL;
    otel_manual_metric_reader_t* reader = NULL;
    otel_sdk_builder_t* sdk_builder = NULL;
    otel_sdk_t* sdk = NULL;
    otel_meter_provider_t* provider = NULL;
    otel_meter_t* meter = NULL;

    otel_counter_u64_t* counter = NULL;
    otel_up_down_counter_i64_t* up_down_counter = NULL;
    otel_gauge_f64_t* gauge = NULL;
    otel_histogram_f64_t* histogram = NULL;
    otel_bound_counter_u64_t* bound_counter = NULL;
    otel_bound_histogram_f64_t* bound_histogram = NULL;
    otel_observable_counter_u64_t* observable_counter = NULL;
    otel_observable_up_down_counter_i64_t* observable_up_down = NULL;
    otel_observable_gauge_f64_t* observable_gauge = NULL;

    observable_u64_state_t* observable_counter_state = NULL;
    observable_i64_state_t* observable_up_down_state = NULL;
    observable_f64_state_t* observable_gauge_state = NULL;
    otel_status_t status;

    memset(&exporter_state, 0, sizeof(exporter_state));
    memset(&callbacks, 0, sizeof(callbacks));
    callbacks.struct_size = sizeof(callbacks);
    callbacks.export_metrics = export_metrics;

    status = otel_custom_metric_exporter_new(
        &callbacks, &exporter_state, OTEL_METRIC_TEMPORALITY_CUMULATIVE, &exporter);
    if (status != OTEL_STATUS_OK) {
        print_status_error("otel_custom_metric_exporter_new", status);
        goto cleanup;
    }
    status = otel_manual_metric_reader_new(exporter, &reader);
    if (status != OTEL_STATUS_OK) {
        print_status_error("otel_manual_metric_reader_new", status);
        goto cleanup;
    }
    exporter = NULL;

    sdk_builder = otel_sdk_builder_new();
    if (sdk_builder == NULL) {
        fprintf(stderr, "otel_sdk_builder_new failed\n");
        goto cleanup;
    }
    status = otel_sdk_builder_set_service_name(sdk_builder, otel_cstr("c-metrics-instruments"));
    if (status != OTEL_STATUS_OK) {
        print_status_error("otel_sdk_builder_set_service_name", status);
        goto cleanup;
    }
    status = otel_sdk_builder_add_manual_metric_reader(sdk_builder, reader);
    if (status != OTEL_STATUS_OK) {
        print_status_error("otel_sdk_builder_add_manual_metric_reader", status);
        goto cleanup;
    }
    reader = NULL;

    status = otel_sdk_build(sdk_builder, &sdk);
    if (status != OTEL_STATUS_OK || sdk == NULL) {
        print_status_error("otel_sdk_build", status);
        goto cleanup;
    }
    otel_sdk_builder_destroy(sdk_builder);
    sdk_builder = NULL;
    status = otel_sdk_set_metrics_as_global(sdk);
    if (status != OTEL_STATUS_OK) {
        print_status_error("otel_sdk_set_metrics_as_global", status);
        goto cleanup;
    }

    provider = otel_global_meter_provider();
    meter = otel_meter_provider_get_meter(
        provider, otel_cstr("example.instruments"), otel_cstr("1.0.0"), otel_string_view_empty());
    if (provider == NULL || meter == NULL) {
        fprintf(stderr, "failed to acquire provider/meter\n");
        goto cleanup;
    }

    otel_instrument_options_t options = OTEL_INSTRUMENT_OPTIONS_INIT;
    options.unit = otel_cstr("1");
    options.description = otel_cstr("Completed requests (counter)");
    status = otel_meter_create_u64_counter(meter, otel_cstr("requests.completed"), &options, &counter);
    if (status != OTEL_STATUS_OK || counter == NULL) {
        print_status_error("otel_meter_create_u64_counter", status);
        goto cleanup;
    }

    options.description = otel_cstr("Active connections (up-down counter)");
    status = otel_meter_create_i64_up_down_counter(
        meter, otel_cstr("connections.active"), &options, &up_down_counter);
    if (status != OTEL_STATUS_OK || up_down_counter == NULL) {
        print_status_error("otel_meter_create_i64_up_down_counter", status);
        goto cleanup;
    }

    options.description = otel_cstr("Queue depth (gauge)");
    status = otel_meter_create_f64_gauge(meter, otel_cstr("queue.depth"), &options, &gauge);
    if (status != OTEL_STATUS_OK || gauge == NULL) {
        print_status_error("otel_meter_create_f64_gauge", status);
        goto cleanup;
    }

    double boundaries[] = { 5.0, 10.0, 25.0, 50.0 };
    otel_instrument_options_t histogram_options = options;
    histogram_options.description = otel_cstr("Request duration (histogram)");
    histogram_options.unit = otel_cstr("ms");
    histogram_options.boundaries = boundaries;
    histogram_options.boundary_count = sizeof(boundaries) / sizeof(boundaries[0]);
    status = otel_meter_create_f64_histogram(
        meter, otel_cstr("request.duration.ms"), &histogram_options, &histogram);
    if (status != OTEL_STATUS_OK || histogram == NULL) {
        print_status_error("otel_meter_create_f64_histogram", status);
        goto cleanup;
    }

    otel_key_value_t attrs[] = {
        otel_kv_string(otel_cstr("endpoint"), otel_cstr("/items")),
        otel_kv_string(otel_cstr("region"), otel_cstr("us-east"))
    };
    status = otel_counter_u64_add(counter, 10, attrs, 2);
    if (status != OTEL_STATUS_OK) {
        print_status_error("otel_counter_u64_add", status);
        goto cleanup;
    }
    status = otel_up_down_counter_i64_add(up_down_counter, 3, attrs, 2);
    if (status != OTEL_STATUS_OK) {
        print_status_error("otel_up_down_counter_i64_add", status);
        goto cleanup;
    }
    status = otel_up_down_counter_i64_add(up_down_counter, -1, attrs, 2);
    if (status != OTEL_STATUS_OK) {
        print_status_error("otel_up_down_counter_i64_add(-1)", status);
        goto cleanup;
    }
    status = otel_gauge_f64_record(gauge, 7.0, attrs, 2);
    if (status != OTEL_STATUS_OK) {
        print_status_error("otel_gauge_f64_record", status);
        goto cleanup;
    }
    status = otel_histogram_f64_record(histogram, 18.5, attrs, 2);
    if (status != OTEL_STATUS_OK) {
        print_status_error("otel_histogram_f64_record", status);
        goto cleanup;
    }

    /* Hot-loop pattern: bind stable attributes once, then record without re-converting them. */
    status = otel_counter_u64_bind(counter, attrs, 2, &bound_counter);
    if (status != OTEL_STATUS_OK || bound_counter == NULL) {
        print_status_error("otel_counter_u64_bind", status);
        goto cleanup;
    }
    status = otel_bound_counter_u64_add(bound_counter, 2);
    if (status != OTEL_STATUS_OK) {
        print_status_error("otel_bound_counter_u64_add", status);
        goto cleanup;
    }

    status = otel_histogram_f64_bind(histogram, attrs, 2, &bound_histogram);
    if (status != OTEL_STATUS_OK || bound_histogram == NULL) {
        print_status_error("otel_histogram_f64_bind", status);
        goto cleanup;
    }
    status = otel_bound_histogram_f64_record(bound_histogram, 12.0);
    if (status != OTEL_STATUS_OK) {
        print_status_error("otel_bound_histogram_f64_record", status);
        goto cleanup;
    }

    observable_counter_state = (observable_u64_state_t*)malloc(sizeof(*observable_counter_state));
    observable_up_down_state = (observable_i64_state_t*)malloc(sizeof(*observable_up_down_state));
    observable_gauge_state = (observable_f64_state_t*)malloc(sizeof(*observable_gauge_state));
    if (observable_counter_state == NULL || observable_up_down_state == NULL ||
        observable_gauge_state == NULL) {
        fprintf(stderr, "failed to allocate observable callback state\n");
        goto cleanup;
    }
    observable_counter_state->value = 5;
    observable_up_down_state->value = -2;
    observable_gauge_state->value = 6.5;

    options.description = otel_cstr("Items waiting for processing (observable counter)");
    status = otel_meter_create_u64_observable_counter(
        meter, otel_cstr("items.waiting"), &options, observe_u64, observable_counter_state,
        destroy_observable_u64_state, &observable_counter);
    if (status != OTEL_STATUS_OK || observable_counter == NULL) {
        print_status_error("otel_meter_create_u64_observable_counter", status);
        goto cleanup;
    }
    observable_counter_state = NULL;

    options.description = otel_cstr("Background workers in use (observable up-down counter)");
    status = otel_meter_create_i64_observable_up_down_counter(
        meter, otel_cstr("workers.in_use"), &options, observe_i64, observable_up_down_state,
        destroy_observable_i64_state, &observable_up_down);
    if (status != OTEL_STATUS_OK || observable_up_down == NULL) {
        print_status_error("otel_meter_create_i64_observable_up_down_counter", status);
        goto cleanup;
    }
    observable_up_down_state = NULL;

    options.description = otel_cstr("Memory pressure (observable gauge)");
    status = otel_meter_create_f64_observable_gauge(
        meter, otel_cstr("memory.pressure"), &options, observe_f64, observable_gauge_state,
        destroy_observable_f64_state, &observable_gauge);
    if (status != OTEL_STATUS_OK || observable_gauge == NULL) {
        print_status_error("otel_meter_create_f64_observable_gauge", status);
        goto cleanup;
    }
    observable_gauge_state = NULL;

    status = otel_sdk_metrics_force_flush(sdk, 0);
    if (status != OTEL_STATUS_OK) {
        print_status_error("otel_sdk_metrics_force_flush", status);
        goto cleanup;
    }
    if (exporter_state.exports < 1 || exporter_state.metric_callbacks < 7) {
        fprintf(stderr, "unexpected export coverage: exports=%d metric_callbacks=%d\n",
                exporter_state.exports,
                exporter_state.metric_callbacks);
        goto cleanup;
    }

    status = otel_sdk_metrics_shutdown(sdk, 5000);
    if (status != OTEL_STATUS_OK) {
        print_status_error("otel_sdk_metrics_shutdown", status);
        goto cleanup;
    }
    printf("instruments example exported %d batch(es) with %d metric callbacks\n",
           exporter_state.exports,
           exporter_state.metric_callbacks);
    result = 0;

cleanup:
    otel_observable_gauge_f64_destroy(observable_gauge);
    otel_observable_up_down_counter_i64_destroy(observable_up_down);
    otel_observable_counter_u64_destroy(observable_counter);
    otel_bound_histogram_f64_destroy(bound_histogram);
    otel_bound_counter_u64_destroy(bound_counter);
    otel_histogram_f64_destroy(histogram);
    otel_gauge_f64_destroy(gauge);
    otel_up_down_counter_i64_destroy(up_down_counter);
    otel_counter_u64_destroy(counter);
    otel_meter_destroy(meter);
    otel_meter_provider_destroy(provider);
    otel_sdk_destroy(sdk);
    otel_sdk_builder_destroy(sdk_builder);
    otel_manual_metric_reader_destroy(reader);
    otel_metric_exporter_destroy(exporter);
    free(observable_counter_state);
    free(observable_up_down_state);
    free(observable_gauge_state);
    return result;
}
