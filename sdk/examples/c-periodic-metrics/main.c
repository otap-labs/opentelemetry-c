#include <opentelemetry_c/metric_exporter.h>
#include <opentelemetry_c/metrics.h>
#include <opentelemetry_c/periodic_metric_reader.h>
#include <opentelemetry_c/sdk.h>

#include <stdatomic.h>
#include <stdio.h>
#include <string.h>
#include <time.h>

typedef struct periodic_state_t {
    atomic_uint exports;
    atomic_uint saw_metric;
} periodic_state_t;

static void sleep_millis(uint64_t millis) {
    struct timespec req;
    req.tv_sec = (time_t)(millis / 1000);
    req.tv_nsec = (long)((millis % 1000) * 1000000u);
    while (nanosleep(&req, &req) != 0) {
    }
}

static void print_status_error(const char* what, otel_status_t status) {
    otel_string_view_t message = otel_last_error_message();
    fprintf(stderr, "%s failed: status=%u", what, (unsigned)status);
    if (message.ptr != NULL && message.len > 0) {
        fprintf(stderr, " (%.*s)", (int)message.len, message.ptr);
    }
    fputc('\n', stderr);
}

static otel_status_t visit_metric(void* user_data, const otel_metric_metadata_t* metadata) {
    periodic_state_t* state = (periodic_state_t*)user_data;
    if (metadata == NULL) {
        return OTEL_STATUS_INVALID_ARGUMENT;
    }
    if (metadata->name.len == strlen("jobs.processed") &&
        memcmp(metadata->name.ptr, "jobs.processed", metadata->name.len) == 0) {
        atomic_store_explicit(&state->saw_metric, 1u, memory_order_relaxed);
    }
    return OTEL_STATUS_OK;
}

static otel_status_t export_metrics(void* user_data, const otel_metric_batch_t* batch) {
    periodic_state_t* state = (periodic_state_t*)user_data;
    otel_metric_visitor_t visitor;
    memset(&visitor, 0, sizeof(visitor));
    visitor.struct_size = sizeof(visitor);
    visitor.metric = visit_metric;
    atomic_fetch_add_explicit(&state->exports, 1u, memory_order_relaxed);
    if (batch == NULL) {
        return OTEL_STATUS_INVALID_ARGUMENT;
    }
    return otel_metric_batch_visit(batch, &visitor, user_data);
}

int main(void) {
    int result = 1;
    periodic_state_t state;
    otel_custom_metric_exporter_callbacks_t callbacks;
    otel_metric_exporter_t* exporter = NULL;
    otel_periodic_metric_reader_builder_t* reader_builder = NULL;
    otel_periodic_metric_reader_t* reader = NULL;
    otel_sdk_builder_t* sdk_builder = NULL;
    otel_sdk_t* sdk = NULL;
    otel_meter_provider_t* provider = NULL;
    otel_meter_t* meter = NULL;
    otel_counter_u64_t* counter = NULL;
    otel_status_t status;

    atomic_init(&state.exports, 0u);
    atomic_init(&state.saw_metric, 0u);
    memset(&callbacks, 0, sizeof(callbacks));
    callbacks.struct_size = sizeof(callbacks);
    callbacks.export_metrics = export_metrics;

    status = otel_custom_metric_exporter_new(
        &callbacks, &state, OTEL_METRIC_TEMPORALITY_CUMULATIVE, &exporter);
    if (status != OTEL_STATUS_OK) {
        print_status_error("otel_custom_metric_exporter_new", status);
        goto cleanup;
    }

    reader_builder = otel_periodic_metric_reader_builder_new();
    if (reader_builder == NULL) {
        fprintf(stderr, "otel_periodic_metric_reader_builder_new failed\n");
        goto cleanup;
    }
    status = otel_periodic_metric_reader_builder_set_interval_millis(reader_builder, 100);
    if (status != OTEL_STATUS_OK) {
        print_status_error("otel_periodic_metric_reader_builder_set_interval_millis", status);
        goto cleanup;
    }
    status = otel_periodic_metric_reader_builder_set_exporter(reader_builder, exporter);
    if (status != OTEL_STATUS_OK) {
        print_status_error("otel_periodic_metric_reader_builder_set_exporter", status);
        goto cleanup;
    }
    exporter = NULL;
    status = otel_periodic_metric_reader_builder_build(reader_builder, &reader);
    if (status != OTEL_STATUS_OK || reader == NULL) {
        print_status_error("otel_periodic_metric_reader_builder_build", status);
        goto cleanup;
    }
    otel_periodic_metric_reader_builder_destroy(reader_builder);
    reader_builder = NULL;

    sdk_builder = otel_sdk_builder_new();
    if (sdk_builder == NULL) {
        fprintf(stderr, "otel_sdk_builder_new failed\n");
        goto cleanup;
    }
    status = otel_sdk_builder_set_service_name(sdk_builder, otel_cstr("c-periodic-metrics"));
    if (status != OTEL_STATUS_OK) {
        print_status_error("otel_sdk_builder_set_service_name", status);
        goto cleanup;
    }
    status = otel_sdk_builder_add_metric_reader(sdk_builder, reader);
    if (status != OTEL_STATUS_OK) {
        print_status_error("otel_sdk_builder_add_metric_reader", status);
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
        provider, otel_cstr("example.periodic"), otel_cstr("1.0.0"), otel_string_view_empty());
    if (provider == NULL || meter == NULL) {
        fprintf(stderr, "failed to acquire provider/meter\n");
        goto cleanup;
    }

    otel_instrument_options_t options = OTEL_INSTRUMENT_OPTIONS_INIT;
    options.description = otel_cstr("Number of processed jobs");
    options.unit = otel_cstr("1");
    status = otel_meter_create_u64_counter(meter, otel_cstr("jobs.processed"), &options, &counter);
    if (status != OTEL_STATUS_OK || counter == NULL) {
        print_status_error("otel_meter_create_u64_counter", status);
        goto cleanup;
    }

    otel_key_value_t attrs[] = {
        otel_kv_string(otel_cstr("worker_pool"), otel_cstr("default")),
        otel_kv_string(otel_cstr("result"), otel_cstr("success"))
    };
    for (size_t i = 0; i < 3; i++) {
        status = otel_counter_u64_add(counter, 1, attrs, 2);
        if (status != OTEL_STATUS_OK) {
            print_status_error("otel_counter_u64_add", status);
            goto cleanup;
        }
        sleep_millis(120);
    }

    const unsigned int target_exports = 2;
    for (size_t i = 0; i < 40; i++) {
        unsigned int observed =
            atomic_load_explicit(&state.exports, memory_order_relaxed);
        if (observed >= target_exports) {
            break;
        }
        sleep_millis(50);
    }
    if (atomic_load_explicit(&state.exports, memory_order_relaxed) < target_exports ||
        atomic_load_explicit(&state.saw_metric, memory_order_relaxed) == 0) {
        fprintf(stderr, "periodic export did not trigger as expected (exports=%u saw=%u)\n",
                atomic_load_explicit(&state.exports, memory_order_relaxed),
                atomic_load_explicit(&state.saw_metric, memory_order_relaxed));
        goto cleanup;
    }

    status = otel_sdk_metrics_force_flush(sdk, 0);
    if (status != OTEL_STATUS_OK) {
        print_status_error("otel_sdk_metrics_force_flush", status);
        goto cleanup;
    }
    status = otel_sdk_metrics_shutdown(sdk, 5000);
    if (status != OTEL_STATUS_OK) {
        print_status_error("otel_sdk_metrics_shutdown", status);
        goto cleanup;
    }
    printf("periodic reader exported %u batch(es)\n",
           atomic_load_explicit(&state.exports, memory_order_relaxed));
    result = 0;

cleanup:
    otel_counter_u64_destroy(counter);
    otel_meter_destroy(meter);
    otel_meter_provider_destroy(provider);
    otel_sdk_destroy(sdk);
    otel_sdk_builder_destroy(sdk_builder);
    otel_periodic_metric_reader_destroy(reader);
    otel_periodic_metric_reader_builder_destroy(reader_builder);
    otel_metric_exporter_destroy(exporter);
    return result;
}
