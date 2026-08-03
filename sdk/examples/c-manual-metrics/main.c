// SPDX-License-Identifier: Apache-2.0

#include <opentelemetry_c/manual_metric_reader.h>
#include <opentelemetry_c/metric_exporter.h>
#include <opentelemetry_c/metrics.h>
#include <opentelemetry_c/sdk.h>

#include <inttypes.h>
#include <stdio.h>
#include <string.h>

typedef struct manual_state_t {
    int exports;
    int flushes;
    uint64_t last_value;
    uint32_t current_data_kind;
    uint32_t current_number_kind;
} manual_state_t;

static void print_status_error(const char* what, otel_status_t status) {
    otel_string_view_t message = otel_last_error_message();
    fprintf(stderr, "%s failed: status=%u", what, (unsigned)status);
    if (message.ptr != NULL && message.len > 0) {
        fprintf(stderr, " (%.*s)", (int)message.len, message.ptr);
    }
    fputc('\n', stderr);
}

static otel_status_t visit_metric(void* user_data, const otel_metric_metadata_t* metadata) {
    manual_state_t* state = (manual_state_t*)user_data;
    if (metadata == NULL) {
        return OTEL_STATUS_INVALID_ARGUMENT;
    }
    state->current_data_kind = metadata->data_kind;
    state->current_number_kind = metadata->number_kind;
    return OTEL_STATUS_OK;
}

static otel_status_t visit_point(void* user_data,
                                 const otel_metric_point_t* point,
                                 const otel_metric_attribute_t* attributes,
                                 size_t attribute_count,
                                 const double* explicit_bounds,
                                 size_t explicit_bound_count,
                                 const uint64_t* explicit_bucket_counts,
                                 size_t explicit_bucket_count,
                                 const uint64_t* positive_bucket_counts,
                                 size_t positive_bucket_count,
                                 const uint64_t* negative_bucket_counts,
                                 size_t negative_bucket_count) {
    manual_state_t* state = (manual_state_t*)user_data;
    (void)attributes;
    (void)attribute_count;
    (void)explicit_bounds;
    (void)explicit_bound_count;
    (void)explicit_bucket_counts;
    (void)explicit_bucket_count;
    (void)positive_bucket_counts;
    (void)positive_bucket_count;
    (void)negative_bucket_counts;
    (void)negative_bucket_count;
    if (point == NULL) {
        return OTEL_STATUS_INVALID_ARGUMENT;
    }
    if (state->current_data_kind == OTEL_METRIC_DATA_SUM &&
        state->current_number_kind == OTEL_METRIC_NUMBER_U64) {
        state->last_value = point->value.u64_value;
    }
    return OTEL_STATUS_OK;
}

static otel_status_t export_metrics(void* user_data, const otel_metric_batch_t* batch) {
    manual_state_t* state = (manual_state_t*)user_data;
    otel_metric_visitor_t visitor;
    memset(&visitor, 0, sizeof(visitor));
    visitor.struct_size = sizeof(visitor);
    visitor.metric = visit_metric;
    visitor.point = visit_point;
    state->exports += 1;
    if (batch == NULL) {
        return OTEL_STATUS_INVALID_ARGUMENT;
    }
    return otel_metric_batch_visit(batch, &visitor, user_data);
}

static otel_status_t exporter_force_flush(void* user_data) {
    manual_state_t* state = (manual_state_t*)user_data;
    state->flushes += 1;
    return OTEL_STATUS_OK;
}

int main(void) {
    int result = 1;
    manual_state_t state;
    otel_custom_metric_exporter_callbacks_t callbacks;
    otel_metric_exporter_t* exporter = NULL;
    otel_manual_metric_reader_t* reader = NULL;
    otel_sdk_builder_t* sdk_builder = NULL;
    otel_sdk_t* sdk = NULL;
    otel_meter_provider_t* provider = NULL;
    otel_meter_t* meter = NULL;
    otel_counter_u64_t* counter = NULL;
    otel_status_t status;

    memset(&state, 0, sizeof(state));
    memset(&callbacks, 0, sizeof(callbacks));
    callbacks.struct_size = sizeof(callbacks);
    callbacks.export_metrics = export_metrics;
    callbacks.force_flush = exporter_force_flush;

    status = otel_custom_metric_exporter_new(
        &callbacks, &state, OTEL_METRIC_TEMPORALITY_CUMULATIVE, &exporter);
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
    status = otel_sdk_builder_set_service_name(sdk_builder, otel_cstr("c-manual-metrics"));
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
        provider, otel_cstr("example.manual"), otel_cstr("1.0.0"), otel_string_view_empty());
    if (provider == NULL || meter == NULL) {
        fprintf(stderr, "failed to acquire provider/meter\n");
        goto cleanup;
    }

    otel_instrument_options_t options = OTEL_INSTRUMENT_OPTIONS_INIT;
    options.description = otel_cstr("Operations completed");
    options.unit = otel_cstr("1");
    status = otel_meter_create_u64_counter(meter, otel_cstr("ops.completed"), &options, &counter);
    if (status != OTEL_STATUS_OK || counter == NULL) {
        print_status_error("otel_meter_create_u64_counter", status);
        goto cleanup;
    }

    otel_key_value_t attrs[] = {
        otel_kv_string(otel_cstr("component"), otel_cstr("cli")),
        otel_kv_string(otel_cstr("phase"), otel_cstr("checkpoint"))
    };

    status = otel_counter_u64_add(counter, 3, attrs, 2);
    if (status != OTEL_STATUS_OK) {
        print_status_error("otel_counter_u64_add(3)", status);
        goto cleanup;
    }
    status = otel_sdk_metrics_force_flush(sdk, 2500);
    if (status != OTEL_STATUS_OK) {
        print_status_error("otel_sdk_metrics_force_flush(first)", status);
        goto cleanup;
    }
    if (state.exports < 1 || state.last_value < 3) {
        fprintf(stderr, "first checkpoint did not export expected value\n");
        goto cleanup;
    }

    status = otel_counter_u64_add(counter, 4, attrs, 2);
    if (status != OTEL_STATUS_OK) {
        print_status_error("otel_counter_u64_add(4)", status);
        goto cleanup;
    }
    status = otel_sdk_metrics_force_flush(sdk, 1);
    if (status != OTEL_STATUS_OK) {
        print_status_error("otel_sdk_metrics_force_flush(second)", status);
        goto cleanup;
    }
    if (state.exports < 2 || state.flushes < 2 || state.last_value < 7) {
        fprintf(stderr,
                "second checkpoint did not export expected cumulative value: "
                "exports=%d flushes=%d last=%" PRIu64 "\n",
                state.exports,
                state.flushes,
                state.last_value);
        goto cleanup;
    }

    status = otel_sdk_metrics_shutdown(sdk, 5000);
    if (status != OTEL_STATUS_OK) {
        print_status_error("otel_sdk_metrics_shutdown", status);
        goto cleanup;
    }
    printf("manual reader exported %d batches; last cumulative value=%" PRIu64 "\n",
           state.exports,
           state.last_value);
    result = 0;

cleanup:
    otel_counter_u64_destroy(counter);
    otel_meter_destroy(meter);
    otel_meter_provider_destroy(provider);
    otel_sdk_destroy(sdk);
    otel_sdk_builder_destroy(sdk_builder);
    otel_manual_metric_reader_destroy(reader);
    otel_metric_exporter_destroy(exporter);
    return result;
}
