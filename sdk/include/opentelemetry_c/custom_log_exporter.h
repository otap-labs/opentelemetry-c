#ifndef OPENTELEMETRY_C_CUSTOM_LOG_EXPORTER_H
#define OPENTELEMETRY_C_CUSTOM_LOG_EXPORTER_H

#include <opentelemetry_c/log_exporter.h>
#include <opentelemetry_c/logs.h>

#ifdef __cplusplus
extern "C" {
#endif

/*
 * Callback-backed Logs exporter.
 *
 * The export callback receives a flattened, borrowed, read-only view of one batch. The view
 * deliberately reuses otel_log_value_t / otel_log_key_value_t and the flat node-pool
 * invariants documented in <opentelemetry_c/logs.h>, so a bridge can share one traversal
 * routine between the emit path and the export path.
 *
 * ---- Lifetime -------------------------------------------------------------
 *
 * Every pointer reachable from otel_log_export_batch_view_t is valid only for the duration of
 * the export callback. Storing any of them, or any otel_string_view_t / otel_log_bytes_view_t
 * they contain, and reading them after the callback returns is undefined behaviour. Copy what
 * you need before returning.
 *
 * ---- Callback thread and concurrency --------------------------------------
 *
 * The callback runs on whichever thread the configured log processor uses:
 *   - simple processor: the thread that emitted the record; the processor serializes calls
 *     with its own mutex, so exports never overlap;
 *   - batch processor: the processor's dedicated worker thread.
 * Callback state must still be safe to use from a thread other than the one that created it,
 * and must tolerate a shutdown callback arriving on a different thread than the exports.
 *
 * ---- Reentrancy -----------------------------------------------------------
 *
 * Both processors export inside a telemetry-suppressed scope, so emitting a log record from
 * inside the callback is silently dropped rather than recursing. Callbacks must NOT shut down
 * or destroy the SDK, logger provider, log processor, or exporter that is currently invoking
 * them: the simple processor holds its exporter lock and the exporter holds its own shutdown
 * lock across the callback, so such a call self-deadlocks.
 *
 * ---- Ownership ------------------------------------------------------------
 *
 * user_data stays caller-owned until otel_custom_log_exporter_new returns OTEL_STATUS_OK. On
 * success the SDK owns it and invokes state_destroy exactly once, after every in-flight
 * export callback has returned. On failure state_destroy is not invoked and the caller must
 * release the state itself.
 *
 * ---- Statuses -------------------------------------------------------------
 *
 * export_logs and shutdown should return OTEL_STATUS_OK, OTEL_STATUS_EXPORT_FAILED,
 * OTEL_STATUS_TIMEOUT, OTEL_STATUS_ALREADY_SHUTDOWN or OTEL_STATUS_INTERNAL_ERROR. Any other
 * status is treated as a callback contract violation and reported as an internal failure.
 * A non-zero status fails that export and surfaces as OTEL_STATUS_EXPORT_FAILED at the
 * provider force-flush/shutdown boundary.
 *
 * There is deliberately no force-flush callback: the underlying Rust LogExporter trait has no
 * force-flush operation, so the SDK would never invoke one. Provider force-flush is handled
 * entirely by the log processor.
 */

/* ---- present_fields bits --------------------------------------------------
 *
 * Unset bits mean the upstream record carried nothing for that field; the corresponding
 * struct member is then zeroed/empty and must not be interpreted as data. Consumers must
 * ignore bits outside OTEL_LOG_EXPORT_FIELD_KNOWN_MASK so newer SDKs stay compatible.
 */
#define OTEL_LOG_EXPORT_FIELD_TIMESTAMP ((uint64_t)1 << 0)
#define OTEL_LOG_EXPORT_FIELD_OBSERVED_TIMESTAMP ((uint64_t)1 << 1)
#define OTEL_LOG_EXPORT_FIELD_TRACE_CONTEXT ((uint64_t)1 << 2)
#define OTEL_LOG_EXPORT_FIELD_SEVERITY_TEXT ((uint64_t)1 << 3)
#define OTEL_LOG_EXPORT_FIELD_EVENT_NAME ((uint64_t)1 << 4)
#define OTEL_LOG_EXPORT_FIELD_TARGET ((uint64_t)1 << 5)
#define OTEL_LOG_EXPORT_FIELD_BODY ((uint64_t)1 << 6)
#define OTEL_LOG_EXPORT_FIELD_KNOWN_MASK                                     \
    (OTEL_LOG_EXPORT_FIELD_TIMESTAMP | OTEL_LOG_EXPORT_FIELD_OBSERVED_TIMESTAMP | \
     OTEL_LOG_EXPORT_FIELD_TRACE_CONTEXT | OTEL_LOG_EXPORT_FIELD_SEVERITY_TEXT |  \
     OTEL_LOG_EXPORT_FIELD_EVENT_NAME | OTEL_LOG_EXPORT_FIELD_TARGET |            \
     OTEL_LOG_EXPORT_FIELD_BODY)

/* A batch larger than this fails the export instead of being silently truncated. */
#define OTEL_LOG_EXPORT_MAX_RECORDS ((size_t)65536)

/*
 * Instrumentation scope shared by one or more records of a batch. Scopes are deduplicated per
 * batch, so several records may point at the same view.
 *
 * attributes[i] is a keyed value whose ARRAY/MAP children are indices into value_nodes, which
 * is this scope's own pool. The pool obeys the same invariants as the emit path: children of
 * a node occupy a contiguous range at strictly greater indices, and every node belongs to
 * exactly one parent.
 */
typedef struct otel_log_export_scope_view_t {
    uint64_t struct_size;
    otel_string_view_t name;
    otel_string_view_t version;
    otel_string_view_t schema_url;
    const otel_log_key_value_t* attributes;
    size_t attribute_count;
    const otel_log_key_value_t* value_nodes;
    size_t value_node_count;
    uint64_t reserved[2];
} otel_log_export_scope_view_t;

/*
 * One exported record. attributes preserve upstream order and may repeat a key; a bridge that
 * needs uniqueness must apply its own last-wins or first-wins policy.
 *
 * Unlike the emit path, exported MAP keys are reproduced verbatim and may be empty: the read
 * path must never rewrite legal upstream data.
 *
 * severity_number is 0 when absent, otherwise a canonical otel_log_severity_t value.
 */
typedef struct otel_log_export_record_view_t {
    uint64_t struct_size;
    uint64_t present_fields;
    uint64_t timestamp_unix_nanos;
    uint64_t observed_timestamp_unix_nanos;
    otel_log_severity_t severity_number;
    uint32_t reserved_flags;
    otel_string_view_t severity_text;
    otel_string_view_t event_name;
    otel_string_view_t target;
    otel_log_value_t body;
    const otel_log_key_value_t* attributes;
    size_t attribute_count;
    const otel_log_key_value_t* value_nodes;
    size_t value_node_count;
    otel_log_trace_context_t trace_context;
    const otel_log_export_scope_view_t* scope;
    uint64_t reserved[4];
} otel_log_export_record_view_t;

/*
 * One exported batch. Resource attributes are shared by every record and live in their own
 * node pool.
 */
typedef struct otel_log_export_batch_view_t {
    uint64_t struct_size;
    const otel_log_export_record_view_t* records;
    size_t record_count;
    otel_string_view_t resource_schema_url;
    const otel_log_key_value_t* resource_attributes;
    size_t resource_attribute_count;
    const otel_log_key_value_t* resource_value_nodes;
    size_t resource_value_node_count;
    uint64_t reserved[4];
} otel_log_export_batch_view_t;

/*
 * export_logs is required. shutdown is optional and is invoked at most once. state_destroy is
 * optional; when NULL the callback state is simply never released by the SDK.
 */
typedef struct otel_custom_log_exporter_callbacks_t {
    size_t struct_size;
    otel_status_t (*export_logs)(void* user_data, const otel_log_export_batch_view_t* batch);
    otel_status_t (*shutdown)(void* user_data, uint64_t timeout_millis);
    void (*state_destroy)(void* user_data);
} otel_custom_log_exporter_callbacks_t;

#if defined(__STDC_VERSION__) && (__STDC_VERSION__ >= 201112L) && \
    defined(UINTPTR_MAX) && (UINTPTR_MAX == 0xFFFFFFFFFFFFFFFFu)
_Static_assert(sizeof(otel_log_export_scope_view_t) == 104,
               "otel_log_export_scope_view_t ABI mismatch");
_Static_assert(sizeof(otel_log_export_record_view_t) == 216,
               "otel_log_export_record_view_t ABI mismatch");
_Static_assert(sizeof(otel_log_export_batch_view_t) == 104,
               "otel_log_export_batch_view_t ABI mismatch");
_Static_assert(sizeof(otel_custom_log_exporter_callbacks_t) == 32,
               "otel_custom_log_exporter_callbacks_t ABI mismatch");
#endif

/*
 * Create a callback-backed Logs exporter. Set callbacks->struct_size to
 * sizeof(otel_custom_log_exporter_callbacks_t).
 *
 * On OTEL_STATUS_OK *out receives a new otel_log_exporter_t and the SDK owns user_data. On
 * failure *out is set to NULL and user_data remains caller-owned.
 */
otel_status_t otel_custom_log_exporter_new(
    const otel_custom_log_exporter_callbacks_t* callbacks,
    void* user_data,
    otel_log_exporter_t** out);

#ifdef __cplusplus
}
#endif
#endif
