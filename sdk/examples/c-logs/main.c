/*
 * C Logs example (EXPERIMENTAL).
 *
 * Builds an OTLP log exporter and a batch log processor, installs the SDK LoggerProvider
 * into the API-owned global Logs slot, emits records through the API-only path, then flushes
 * and shuts down.
 *
 * The emission half of this file deliberately uses ONLY api/logs.h. That is how an
 * instrumentation library behaves: it links the API alone, and its calls are safe no-ops
 * until an application installs an SDK.
 */
#include <opentelemetry_c/logs.h>
#include <opentelemetry_c/log_exporter.h>
#include <opentelemetry_c/log_processor.h>
#include <opentelemetry_c/otlp_log_exporter.h>
#include <opentelemetry_c/sdk.h>

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static otel_string_view_t cs(const char* s) {
    otel_string_view_t view;
    view.ptr = s;
    view.len = s ? strlen(s) : 0;
    return view;
}

static void report(const char* what, otel_status_t status) {
    otel_string_view_t message = otel_last_error_message();
    fprintf(stderr, "%s failed: status=%d %.*s\n", what, (int)status, (int)message.len,
            message.ptr ? (const char*)message.ptr : "");
}

#define TRY(expr)                            \
    do {                                     \
        otel_status_t status_ = (expr);      \
        if (status_ != OTEL_STATUS_OK) {     \
            report(#expr, status_);          \
            return 1;                        \
        }                                    \
    } while (0)

/*
 * Emit one record carrying a structured attribute. Structured values live in a flat "node
 * pool" that the record references by index range, instead of a pointer graph. A node may
 * only reference children at a STRICTLY GREATER index, which makes cycles impossible to
 * express and lets the SDK validate the whole record without a visited set.
 *
 * Pool layout used below:
 *
 *   attributes[0] "http"  --> map over pool[0..2)
 *     pool[0]     "method"  = "GET"
 *     pool[1]     "retries" = array over pool[2..4)
 *       pool[2]              = 10
 *       pool[3]              = 250
 *   attributes[1] "duration_ms" = 12.5
 *
 * Every pointer here is borrowed for the duration of otel_logger_emit() only; the SDK copies
 * everything it keeps before returning.
 */
static int emit_structured(otel_logger_t* logger) {
    otel_log_key_value_t pool[4];
    pool[0] = otel_log_kv(cs("method"), otel_log_value_string(cs("GET")));
    pool[1] = otel_log_kv(cs("retries"), otel_log_value_array(2, 2));
    pool[2] = otel_log_element(otel_log_value_int64(10));
    pool[3] = otel_log_element(otel_log_value_int64(250));

    otel_log_key_value_t attributes[2];
    attributes[0] = otel_log_kv(cs("http"), otel_log_value_map(0, 2));
    attributes[1] = otel_log_kv(cs("duration_ms"), otel_log_value_double(12.5));

    otel_log_record_view_t record = OTEL_LOG_RECORD_VIEW_INIT;
    record.severity_number = OTEL_LOG_SEVERITY_WARN;
    record.body = otel_log_value_string(cs("request retried"));
    record.attributes = attributes;
    record.attribute_count = 2;
    record.value_nodes = pool;
    record.value_node_count = 4;
    TRY(otel_logger_emit(logger, &record));
    return 0;
}

int main(void) {
    const char* endpoint = getenv("OTEL_EXPORTER_OTLP_LOGS_ENDPOINT");
    if (!endpoint) {
        endpoint = "http://localhost:4318/v1/logs";
    }

    /* ---- Application half: build and install the pipeline (needs the SDK). ---- */
    otel_otlp_log_exporter_builder_t* exporter_builder = otel_otlp_log_exporter_builder_new();
    if (!exporter_builder) {
        fprintf(stderr, "failed to allocate the OTLP log exporter builder\n");
        return 1;
    }
    TRY(otel_otlp_log_exporter_builder_set_endpoint(exporter_builder, cs(endpoint)));
    TRY(otel_otlp_log_exporter_builder_set_transport(exporter_builder,
                                                     OTEL_OTLP_LOG_TRANSPORT_HTTP_PROTOBUF));
    otel_log_exporter_t* exporter = NULL;
    TRY(otel_otlp_log_exporter_builder_build(exporter_builder, &exporter));
    otel_otlp_log_exporter_builder_destroy(exporter_builder);

    otel_batch_log_processor_builder_t* processor_builder =
        otel_batch_log_processor_builder_new();
    if (!processor_builder) {
        otel_log_exporter_destroy(exporter);
        fprintf(stderr, "failed to allocate the batch log processor builder\n");
        return 1;
    }
    /* On OTEL_STATUS_OK the exporter is consumed; do not destroy it afterwards. */
    TRY(otel_batch_log_processor_builder_set_exporter(processor_builder, exporter));
    TRY(otel_batch_log_processor_builder_set_max_queue_size(processor_builder, 2048));
    TRY(otel_batch_log_processor_builder_set_max_export_batch_size(processor_builder, 512));
    TRY(otel_batch_log_processor_builder_set_scheduled_delay_millis(processor_builder, 1000));
    otel_log_processor_t* processor = NULL;
    TRY(otel_batch_log_processor_builder_build(processor_builder, &processor));
    otel_batch_log_processor_builder_destroy(processor_builder);

    otel_sdk_builder_t* sdk_builder = otel_sdk_builder_new();
    if (!sdk_builder) {
        otel_log_processor_destroy(processor);
        fprintf(stderr, "failed to allocate the SDK builder\n");
        return 1;
    }
    TRY(otel_sdk_builder_set_service_name(sdk_builder, cs("c-logs-example")));
    TRY(otel_sdk_builder_add_log_processor(sdk_builder, processor));
    otel_sdk_t* sdk = NULL;
    TRY(otel_sdk_build(sdk_builder, &sdk));
    otel_sdk_builder_destroy(sdk_builder);
    TRY(otel_sdk_set_logs_as_global(sdk));

    /* ---- Instrumentation half: API only from here on. ---- */
    otel_logger_provider_t* provider = otel_global_logger_provider();
    if (!provider) {
        fprintf(stderr, "the global logger provider is unavailable\n");
        return 1;
    }
    otel_logger_options_t options = OTEL_LOGGER_OPTIONS_INIT;
    options.name = cs("example-instrumentation");
    options.version = cs("0.1.0");
    otel_logger_t* logger = otel_logger_provider_get_logger_with_options(provider, &options);
    if (!logger) {
        fprintf(stderr, "failed to acquire a logger\n");
        return 1;
    }

    /*
     * Check before building an expensive record. With no SDK installed this returns false and
     * the whole record construction is skipped.
     */
    if (otel_logger_enabled(logger, OTEL_LOG_SEVERITY_INFO)) {
        otel_log_record_view_t record = OTEL_LOG_RECORD_VIEW_INIT;
        record.severity_number = OTEL_LOG_SEVERITY_INFO;
        record.body = otel_log_value_string(cs("service started"));
        TRY(otel_logger_emit(logger, &record));
    }

    if (emit_structured(logger) != 0) {
        return 1;
    }

    /* Correlate a record with an active span by supplying trace context explicitly. */
    otel_log_record_view_t correlated = OTEL_LOG_RECORD_VIEW_INIT;
    correlated.severity_number = OTEL_LOG_SEVERITY_ERROR;
    correlated.body = otel_log_value_string(cs("request failed"));
    correlated.present_fields = OTEL_LOG_FIELD_TRACE_CONTEXT;
    for (int i = 0; i < 16; i++) {
        correlated.trace_context.trace_id[i] = (unsigned char)(i + 1);
    }
    for (int i = 0; i < 8; i++) {
        correlated.trace_context.span_id[i] = (unsigned char)(i + 1);
    }
    correlated.trace_context.trace_flags = OTEL_LOG_TRACE_FLAGS_SAMPLED;
    TRY(otel_logger_emit(logger, &correlated));

    otel_logger_destroy(logger);
    otel_logger_provider_destroy(provider);

    /* ---- Shutdown (application half again). ---- */
    /* Force flush takes no caller-supplied timeout; the pinned batch processor applies its
     * own non-configurable five-second wait. */
    TRY(otel_sdk_logs_force_flush(sdk));
    TRY(otel_sdk_logs_shutdown(sdk, 5000));
    otel_sdk_destroy(sdk);

    printf("emitted 3 log records to %s\n", endpoint);
    return 0;
}
