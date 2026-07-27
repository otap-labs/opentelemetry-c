/*
 * Callback-backed Logs exporter: receive finished log batches in your own C code.
 *
 * This is the whole point of the custom exporter: no OTLP transport, no network, no extra
 * feature flags. The SDK converts each batch into a borrowed, read-only view and hands it to
 * a callback that prints it.
 *
 * Every pointer in the view is valid only for the duration of the callback. This example
 * therefore prints during the callback and copies nothing out of it.
 */

#include <opentelemetry_c/custom_log_exporter.h>
#include <opentelemetry_c/log_processor.h>
#include <opentelemetry_c/logs.h>
#include <opentelemetry_c/sdk.h>

#include <stdint.h>
#include <stdio.h>
#include <string.h>

typedef struct exporter_state_t {
    unsigned long batches;
    unsigned long records;
} exporter_state_t;

static void print_view(otel_string_view_t view) {
    printf("%.*s", (int)view.len, view.len == 0 ? "" : view.ptr);
}

/* Print one value, recursing into the record's flat node pool for containers. */
static void print_value(const otel_log_export_record_view_t* record,
                        const otel_log_value_t* value,
                        int depth) {
    otel_log_value_range_t children;
    size_t i;

    if (depth > (int)OTEL_LOG_MAX_VALUE_DEPTH) {
        printf("<too deep>");
        return;
    }
    switch (value->value_type) {
    case OTEL_LOG_VALUE_TYPE_EMPTY:
        printf("null");
        break;
    case OTEL_LOG_VALUE_TYPE_STRING:
        printf("\"");
        print_view(value->value.string_value);
        printf("\"");
        break;
    case OTEL_LOG_VALUE_TYPE_BOOL:
        printf(value->value.bool_value ? "true" : "false");
        break;
    case OTEL_LOG_VALUE_TYPE_INT64:
        printf("%lld", (long long)value->value.int64_value);
        break;
    case OTEL_LOG_VALUE_TYPE_DOUBLE:
        printf("%f", value->value.double_value);
        break;
    case OTEL_LOG_VALUE_TYPE_BYTES:
        printf("<%zu bytes>", value->value.bytes_value.len);
        break;
    case OTEL_LOG_VALUE_TYPE_ARRAY:
    case OTEL_LOG_VALUE_TYPE_MAP:
        children = value->value.children;
        printf(value->value_type == OTEL_LOG_VALUE_TYPE_ARRAY ? "[" : "{");
        for (i = 0; i < children.count; i++) {
            const otel_log_key_value_t* child = &record->value_nodes[children.first + i];
            if (i > 0) {
                printf(", ");
            }
            if (value->value_type == OTEL_LOG_VALUE_TYPE_MAP) {
                print_view(child->key);
                printf(": ");
            }
            print_value(record, &child->value, depth + 1);
        }
        printf(value->value_type == OTEL_LOG_VALUE_TYPE_ARRAY ? "]" : "}");
        break;
    default:
        printf("<unknown>");
        break;
    }
}

static otel_status_t export_logs(void* user_data, const otel_log_export_batch_view_t* batch) {
    exporter_state_t* state = (exporter_state_t*)user_data;
    size_t i;
    size_t a;

    if (batch == NULL) {
        return OTEL_STATUS_INVALID_ARGUMENT;
    }
    state->batches++;
    printf("--- batch of %zu record(s), %zu resource attribute(s) ---\n",
           batch->record_count,
           batch->resource_attribute_count);

    for (i = 0; i < batch->record_count; i++) {
        const otel_log_export_record_view_t* record = &batch->records[i];
        state->records++;

        printf("  scope=");
        print_view(record->scope->name);
        printf(" severity=%u", (unsigned)record->severity_number);
        if ((record->present_fields & OTEL_LOG_EXPORT_FIELD_TIMESTAMP) != 0) {
            printf(" ts=%llu", (unsigned long long)record->timestamp_unix_nanos);
        }
        if ((record->present_fields & OTEL_LOG_EXPORT_FIELD_TRACE_CONTEXT) != 0) {
            printf(" trace_id=%02x..", record->trace_context.trace_id[0]);
        }
        printf(" body=");
        print_value(record, &record->body, 0);

        for (a = 0; a < record->attribute_count; a++) {
            printf(" ");
            print_view(record->attributes[a].key);
            printf("=");
            print_value(record, &record->attributes[a].value, 0);
        }
        printf("\n");
    }
    return OTEL_STATUS_OK;
}

static otel_status_t shutdown_exporter(void* user_data, uint64_t timeout_millis) {
    exporter_state_t* state = (exporter_state_t*)user_data;
    (void)timeout_millis;
    printf("exporter shutdown after %lu batch(es), %lu record(s)\n", state->batches,
           state->records);
    return OTEL_STATUS_OK;
}

/*
 * Invoked exactly once, after the last export callback has returned. This is where a real
 * bridge would free its callback state; here the state is on the stack of main(), which
 * outlives the SDK, so there is nothing to free.
 */
static void destroy_exporter_state(void* user_data) {
    (void)user_data;
    printf("exporter state released\n");
}

int main(void) {
    exporter_state_t state;
    otel_custom_log_exporter_callbacks_t callbacks;
    otel_log_exporter_t* exporter = NULL;
    otel_log_processor_t* processor = NULL;
    otel_sdk_builder_t* builder = NULL;
    otel_sdk_t* sdk = NULL;
    otel_logger_provider_t* provider = NULL;
    otel_logger_t* logger = NULL;

    otel_log_key_value_t nodes[2];
    otel_log_key_value_t attributes[2];
    otel_log_record_view_t record = OTEL_LOG_RECORD_VIEW_INIT;

    memset(&state, 0, sizeof(state));
    memset(&callbacks, 0, sizeof(callbacks));
    memset(nodes, 0, sizeof(nodes));
    memset(attributes, 0, sizeof(attributes));

    callbacks.struct_size = sizeof(callbacks);
    callbacks.export_logs = export_logs;
    callbacks.shutdown = shutdown_exporter;
    callbacks.state_destroy = destroy_exporter_state;

    /* Ownership of `&state` transfers to the SDK only if this returns OTEL_STATUS_OK. */
    if (otel_custom_log_exporter_new(&callbacks, &state, &exporter) != OTEL_STATUS_OK) {
        fprintf(stderr, "failed to create exporter: %s\n", otel_last_error_message().ptr);
        return 1;
    }

    /* A simple processor exports on the emitting thread, which keeps the output ordered. */
    if (otel_simple_log_processor_create(exporter, &processor) != OTEL_STATUS_OK) {
        fprintf(stderr, "failed to create processor\n");
        otel_log_exporter_destroy(exporter);
        return 1;
    }

    builder = otel_sdk_builder_new();
    otel_sdk_builder_set_service_name(builder, otel_cstr("custom-log-exporter-example"));
    otel_sdk_builder_add_log_processor(builder, processor);
    if (otel_sdk_build(builder, &sdk) != OTEL_STATUS_OK) {
        fprintf(stderr, "failed to build SDK\n");
        otel_sdk_builder_destroy(builder);
        return 1;
    }
    otel_sdk_builder_destroy(builder);

    provider = otel_sdk_get_logger_provider(sdk);
    logger = otel_logger_provider_get_logger(provider, otel_cstr("example"), otel_cstr("1.0.0"),
                                             otel_string_view_empty());

    /* attrs = { "http.method": "GET", "tags": ["a", "b"] } */
    nodes[0].value.value_type = OTEL_LOG_VALUE_TYPE_STRING;
    nodes[0].value.value.string_value = otel_cstr("a");
    nodes[1].value.value_type = OTEL_LOG_VALUE_TYPE_STRING;
    nodes[1].value.value.string_value = otel_cstr("b");

    attributes[0].key = otel_cstr("http.method");
    attributes[0].value.value_type = OTEL_LOG_VALUE_TYPE_STRING;
    attributes[0].value.value.string_value = otel_cstr("GET");
    attributes[1].key = otel_cstr("tags");
    attributes[1].value.value_type = OTEL_LOG_VALUE_TYPE_ARRAY;
    attributes[1].value.value.children.first = 0;
    attributes[1].value.value.children.count = 2;

    record.severity_number = OTEL_LOG_SEVERITY_INFO;
    record.body.value_type = OTEL_LOG_VALUE_TYPE_STRING;
    record.body.value.string_value = otel_cstr("request handled");
    record.attributes = attributes;
    record.attribute_count = 2;
    record.value_nodes = nodes;
    record.value_node_count = 2;

    if (otel_logger_emit(logger, &record) != OTEL_STATUS_OK) {
        fprintf(stderr, "emit failed: %s\n", otel_last_error_message().ptr);
    }

    otel_logger_destroy(logger);
    otel_logger_provider_destroy(provider);

    otel_sdk_logs_force_flush(sdk);
    otel_sdk_logs_shutdown(sdk, 5000);
    otel_sdk_destroy(sdk);
    return 0;
}
