/*
 * opentelemetry_c/logs.h
 *
 * Experimental OpenTelemetry Logs Bridge API.
 *
 * This is a **Logs Bridge** for traditional log records, intended for authors of logging
 * backends and appenders rather than for direct application use. It is NOT an
 * OpenTelemetry Events API: `event_name` is not supported in this version (see the
 * limitations section below).
 *
 * Logger providers and loggers are concurrency-safe. Destroy must not race with another
 * operation on the same handle.
 *
 * Without an installed Logs SDK, logger acquisition succeeds, otel_logger_enabled() returns
 * OTEL_FALSE, and otel_logger_emit() is an allocation-free no-op returning OTEL_STATUS_OK.
 * A logger acquired while no Logs SDK is installed remains no-op; installing an SDK later
 * does not retrofit existing provider or logger handles. Reacquire the provider and the
 * logger after installing the global SDK. This is expected handle behavior.
 *
 * Known limitations of this version
 * ---------------------------------
 *  - `event_name` is NOT supported. The pinned opentelemetry-rust Logs API accepts only
 *    `&'static str` for the event name, and there is no safe way to hand it a C-owned
 *    string without leaking, interning, or lying about the lifetime. The name is not
 *    accepted, not substituted, not smuggled into an attribute/body/scope, and not exposed
 *    as a nonfunctional field. `otel_log_record_view_t` is append-only via `struct_size`, so
 *    event-name support can be added once the Rust SDK offers an owned-value path.
 *  - Caller-supplied severity **text** is NOT accepted, for the same `&'static str` reason.
 *    Instead, this API synthesizes the *canonical* OpenTelemetry severity text from the
 *    normalized severity number (1 => "TRACE", 9 => "INFO", 17 => "ERROR", ...). Original
 *    source spellings such as "WARNING", "ERR", "SEVERE", or localized text are NOT
 *    preserved.
 *  - There is no unsigned 64-bit log value: the pinned Rust `AnyValue` model has no u64
 *    variant. Represent values above INT64_MAX as a string or as bytes.
 *  - `target` is not exposed. Exporters may use it to override the instrumentation scope
 *    name, which would silently corrupt the scope a C caller explicitly supplied.
 */
#ifndef OPENTELEMETRY_C_LOGS_H
#define OPENTELEMETRY_C_LOGS_H

#include "common.h"

#ifdef __cplusplus
extern "C" {
#endif

typedef struct otel_logger_provider_t otel_logger_provider_t;
typedef struct otel_logger_t otel_logger_t;

/* ---- Severity ------------------------------------------------------------- */

/*
 * Canonical OpenTelemetry severity number. Crosses the ABI as a fixed-width uint32_t (not a
 * C enum and not a Rust enum layout) so every bit pattern is well defined.
 *
 * 0 means "absent": neither the severity number nor the severity text is set on the record.
 * 1..24 are the canonical values. Any other value is rejected with
 * OTEL_STATUS_INVALID_ARGUMENT and nothing is emitted.
 */
typedef uint32_t otel_log_severity_t;
enum {
    OTEL_LOG_SEVERITY_UNSPECIFIED = 0,
    OTEL_LOG_SEVERITY_TRACE = 1,
    OTEL_LOG_SEVERITY_TRACE2 = 2,
    OTEL_LOG_SEVERITY_TRACE3 = 3,
    OTEL_LOG_SEVERITY_TRACE4 = 4,
    OTEL_LOG_SEVERITY_DEBUG = 5,
    OTEL_LOG_SEVERITY_DEBUG2 = 6,
    OTEL_LOG_SEVERITY_DEBUG3 = 7,
    OTEL_LOG_SEVERITY_DEBUG4 = 8,
    OTEL_LOG_SEVERITY_INFO = 9,
    OTEL_LOG_SEVERITY_INFO2 = 10,
    OTEL_LOG_SEVERITY_INFO3 = 11,
    OTEL_LOG_SEVERITY_INFO4 = 12,
    OTEL_LOG_SEVERITY_WARN = 13,
    OTEL_LOG_SEVERITY_WARN2 = 14,
    OTEL_LOG_SEVERITY_WARN3 = 15,
    OTEL_LOG_SEVERITY_WARN4 = 16,
    OTEL_LOG_SEVERITY_ERROR = 17,
    OTEL_LOG_SEVERITY_ERROR2 = 18,
    OTEL_LOG_SEVERITY_ERROR3 = 19,
    OTEL_LOG_SEVERITY_ERROR4 = 20,
    OTEL_LOG_SEVERITY_FATAL = 21,
    OTEL_LOG_SEVERITY_FATAL2 = 22,
    OTEL_LOG_SEVERITY_FATAL3 = 23,
    OTEL_LOG_SEVERITY_FATAL4 = 24
};

/* ---- Values (the Logs AnyValue model) ------------------------------------- */

/*
 * Discriminant selecting the active member of otel_log_value_payload_t. Validated before
 * the union is read, so an out-of-range value is rejected with
 * OTEL_STATUS_INVALID_ARGUMENT rather than causing a type-confused read.
 *
 * This model is deliberately wider than otel_attribute_type_t (used by traces and metrics):
 * log bodies and log attributes may be bytes, arrays, or maps.
 */
typedef uint32_t otel_log_value_type_t;
enum {
    OTEL_LOG_VALUE_TYPE_EMPTY = 0,  /* No value. No union member is read. */
    OTEL_LOG_VALUE_TYPE_STRING = 1, /* string_value; must be valid UTF-8. */
    OTEL_LOG_VALUE_TYPE_BOOL = 2,   /* bool_value */
    OTEL_LOG_VALUE_TYPE_INT64 = 3,  /* int64_value */
    OTEL_LOG_VALUE_TYPE_DOUBLE = 4, /* double_value */
    OTEL_LOG_VALUE_TYPE_BYTES = 5,  /* bytes_value; arbitrary bytes, no UTF-8 requirement. */
    OTEL_LOG_VALUE_TYPE_ARRAY = 6,  /* children; array elements in the record node pool. */
    OTEL_LOG_VALUE_TYPE_MAP = 7     /* children; map entries in the record node pool. */
};

/* A borrowed, length-delimited byte string. `ptr` may be NULL only when `len == 0`. */
typedef struct otel_log_bytes_view_t {
    const uint8_t* ptr;
    size_t len;
} otel_log_bytes_view_t;

/*
 * A half-open [first, first + count) range into the record's borrowed node pool
 * (otel_log_record_view_t::value_nodes).
 *
 * Structured values are represented with a FLAT node pool instead of recursive pointers:
 * it makes validation, bounded traversal, and fuzzing tractable, and it makes reference
 * cycles structurally impossible (see the forward-reference rule below).
 */
typedef struct otel_log_value_range_t {
    uint32_t first;
    uint32_t count;
} otel_log_value_range_t;

/* Tagged-union payload for a log value. Set the member matching the tag. */
typedef union otel_log_value_payload_t {
    otel_string_view_t string_value;
    otel_bool_t bool_value;
    int64_t int64_value;
    double double_value;
    otel_log_bytes_view_t bytes_value;
    otel_log_value_range_t children;
} otel_log_value_payload_t;

/* A single log value. `reserved` must be zero. */
typedef struct otel_log_value_t {
    otel_log_value_type_t value_type;
    uint32_t reserved;
    otel_log_value_payload_t value;
} otel_log_value_t;

/*
 * A keyed log value. Used for record attributes, and for entries inside the node pool.
 *
 * In the node pool:
 *   - a node addressed as an ARRAY element must have an empty key;
 *   - a node addressed as a MAP entry must have a non-empty, unique UTF-8 key.
 */
typedef struct otel_log_key_value_t {
    otel_string_view_t key;
    otel_log_value_t value;
} otel_log_key_value_t;

/* ---- Trace correlation ---------------------------------------------------- */

/* The only trace flag defined by W3C Trace Context. Other bits are rejected. */
#define OTEL_LOG_TRACE_FLAGS_SAMPLED ((uint8_t)0x01)
#define OTEL_LOG_TRACE_FLAGS_RANDOM ((uint8_t)0x02)

/*
 * Explicit trace correlation. This is Logs-local and does not require the C Trace API: a
 * caller supplies the identifiers directly and never needs a live span handle.
 *
 * trace_id must not be all zero, span_id must not be all zero, only supported trace-flag
 * bits are accepted, and every reserved byte must be zero. All values are copied during
 * otel_logger_emit().
 */
typedef struct otel_log_trace_context_t {
    uint8_t trace_id[16];
    uint8_t span_id[8];
    uint8_t trace_flags;
    uint8_t reserved[7];
} otel_log_trace_context_t;

/* ---- Record view ---------------------------------------------------------- */

/*
 * Presence bits for otel_log_record_view_t::present_fields.
 *
 * Presence bits exist for fields where an in-band sentinel would be ambiguous: Unix epoch
 * zero is a legitimate instant, and an all-zero trace context is a legitimate bit pattern to
 * receive from a caller that simply did not fill it in. Unknown bits are rejected.
 */
#define OTEL_LOG_FIELD_TIMESTAMP ((uint64_t)1 << 0)
#define OTEL_LOG_FIELD_OBSERVED_TIMESTAMP ((uint64_t)1 << 1)
#define OTEL_LOG_FIELD_TRACE_CONTEXT ((uint64_t)1 << 2)

/*
 * Mandatory per-record limits. All are enforced before any large allocation or unsafe slice
 * construction. Exceeding one returns OTEL_STATUS_INVALID_ARGUMENT and emits nothing.
 */
#define OTEL_LOG_MAX_ATTRIBUTES ((size_t)256)
#define OTEL_LOG_MAX_VALUE_NODES ((size_t)1024)
#define OTEL_LOG_MAX_VALUE_DEPTH ((size_t)16)
#define OTEL_LOG_MAX_STRING_LEN ((size_t)1048576)
#define OTEL_LOG_MAX_BYTES_LEN ((size_t)1048576)
#define OTEL_LOG_MAX_MAP_ENTRIES ((size_t)256)
#define OTEL_LOG_MAX_ARRAY_ELEMENTS ((size_t)256)

/*
 * A borrowed, one-shot view of a log record. Initialize with OTEL_LOG_RECORD_VIEW_INIT.
 *
 * EVERY pointer reachable from this structure is borrowed for the duration of the
 * otel_logger_emit() call only. An SDK-backed logger synchronously validates and copies
 * everything it retains before returning; no pointer into caller memory escapes the call.
 * There is deliberately no owned, mutable C log record in this version.
 *
 * Node pool rules (validated by an SDK-backed logger):
 *   - a child range must lie entirely within [0, value_node_count);
 *   - a node in the pool may only reference children at a STRICTLY GREATER index than its
 *     own, which makes cycles impossible without a visited set;
 *   - every node must be referenced EXACTLY ONCE, either by one container or as one
 *     top-level attribute value; unreferenced nodes and shared sub-trees are rejected;
 *   - the total number of converted value nodes for one record is bounded by
 *     OTEL_LOG_MAX_VALUE_NODES;
 *   - traversal is iterative and depth-bounded by OTEL_LOG_MAX_VALUE_DEPTH.
 *
 * Duplicate keys:
 *   - duplicate MAP keys are rejected (the pinned Rust map type would silently drop one);
 *   - duplicate top-level attribute keys are accepted and preserved in order, matching the
 *     pinned Rust log record, which appends attributes without deduplication.
 *
 * `reserved_flags` and every element of `reserved` must be zero.
 */
typedef struct otel_log_record_view_t {
    uint64_t struct_size;
    uint64_t present_fields;
    uint64_t timestamp_unix_nanos;
    uint64_t observed_timestamp_unix_nanos;
    otel_log_severity_t severity_number;
    uint32_t reserved_flags;
    otel_log_value_t body;
    const otel_log_key_value_t* attributes;
    size_t attribute_count;
    const otel_log_key_value_t* value_nodes;
    size_t value_node_count;
    otel_log_trace_context_t trace_context;
    uint64_t reserved[4];
} otel_log_record_view_t;

#define OTEL_LOG_RECORD_VIEW_INIT                                                  \
    {                                                                              \
        sizeof(otel_log_record_view_t), 0, 0, 0, OTEL_LOG_SEVERITY_UNSPECIFIED, 0, \
            {OTEL_LOG_VALUE_TYPE_EMPTY, 0, {{NULL, 0}}}, NULL, 0, NULL, 0,         \
            {{0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0},                     \
             {0, 0, 0, 0, 0, 0, 0, 0},                                             \
             0,                                                                    \
             {0, 0, 0, 0, 0, 0, 0}},                                               \
        {                                                                          \
            0, 0, 0, 0                                                             \
        }                                                                          \
    }

/*
 * Complete instrumentation-scope options for logger acquisition. Initialize with
 * OTEL_LOGGER_OPTIONS_INIT. All strings and attributes are borrowed only for the call and
 * copied by an SDK-backed provider. `name` must be non-empty. Scope attribute keys must be
 * non-empty, valid UTF-8, and unique.
 */
typedef struct otel_logger_options_t {
    uint64_t struct_size;
    otel_string_view_t name;
    otel_string_view_t version;
    otel_string_view_t schema_url;
    const otel_key_value_t* attributes;
    size_t attribute_count;
} otel_logger_options_t;

#define OTEL_LOGGER_OPTIONS_INIT \
    { sizeof(otel_logger_options_t), { NULL, 0 }, { NULL, 0 }, { NULL, 0 }, NULL, 0 }

#if defined(__STDC_VERSION__) && (__STDC_VERSION__ >= 201112L) && \
    defined(UINTPTR_MAX) && (UINTPTR_MAX == 0xFFFFFFFFFFFFFFFFu)
_Static_assert(sizeof(otel_log_bytes_view_t) == 16, "otel_log_bytes_view_t ABI mismatch");
_Static_assert(sizeof(otel_log_value_range_t) == 8, "otel_log_value_range_t ABI mismatch");
_Static_assert(sizeof(otel_log_value_payload_t) == 16, "otel_log_value_payload_t ABI mismatch");
_Static_assert(sizeof(otel_log_value_t) == 24, "otel_log_value_t ABI mismatch");
_Static_assert(sizeof(otel_log_key_value_t) == 40, "otel_log_key_value_t ABI mismatch");
_Static_assert(sizeof(otel_log_trace_context_t) == 32, "otel_log_trace_context_t ABI mismatch");
_Static_assert(sizeof(otel_log_record_view_t) == 160, "otel_log_record_view_t ABI mismatch");
_Static_assert(sizeof(otel_logger_options_t) == 72, "otel_logger_options_t ABI mismatch");
#endif

/* ---- Provider and logger -------------------------------------------------- */

/*
 * Return an owned handle to the process-global LoggerProvider. Release it with
 * otel_logger_provider_destroy() even when no SDK is installed.
 */
otel_logger_provider_t* otel_global_logger_provider(void);

/*
 * Acquire an owned logger. `name` must be non-empty. Returns NULL on failure; call
 * otel_last_error_message() for detail.
 */
otel_logger_t* otel_logger_provider_get_logger(const otel_logger_provider_t* provider,
                                               otel_string_view_t name,
                                               otel_string_view_t version,
                                               otel_string_view_t schema_url);

/* As above, with complete instrumentation-scope options including scope attributes. */
otel_logger_t* otel_logger_provider_get_logger_with_options(
    const otel_logger_provider_t* provider, const otel_logger_options_t* options);

void otel_logger_provider_destroy(otel_logger_provider_t* provider);
void otel_logger_destroy(otel_logger_t* logger);

/*
 * Advisory check for whether a record of `severity` would be processed.
 *
 * This is ADVISORY ONLY. Configuration may change between this call and the emit that
 * follows it, and otel_logger_emit() remains valid whether or not this pre-check was made or
 * returned true. A logger with no installed SDK always returns OTEL_FALSE, as does an
 * absent (0) or out-of-range severity, because the pinned Rust check requires a normalized
 * severity value.
 */
otel_bool_t otel_logger_enabled(const otel_logger_t* logger, otel_log_severity_t severity);

/*
 * Emit one borrowed record.
 *
 * OTEL_STATUS_OK means the record was valid, was successfully converted, and was handed to
 * the configured logger. It does NOT mean a processor accepted it, that a batch queue had
 * room, that an export succeeded, or that a collector received it. The pinned Rust logger
 * emit API returns no delivery status, and this binding does not invent one.
 *
 * Without an installed SDK this returns OTEL_STATUS_OK without allocating and without
 * inspecting any nested borrowed content, so malformed nested values are NOT diagnosed on
 * that path. Only the record handle and header are checked there.
 */
otel_status_t otel_logger_emit(const otel_logger_t* logger,
                               const otel_log_record_view_t* record);

/*
 * Emit a record correlated with an immutable API-owned SpanContext snapshot. The context is
 * borrowed and copied during the call. This fails when `record` already selects its embedded
 * raw trace_context: callers must choose exactly one correlation source. Fields added after
 * the V1 record prefix are not forwarded by this entry point.
 */
otel_status_t otel_logger_emit_with_context(const otel_logger_t* logger,
                                            const otel_log_record_view_t* record,
                                            const otel_span_context_t* context);

/* ---- Helpers -------------------------------------------------------------- */

#if defined(__cplusplus) || (defined(__STDC_VERSION__) && __STDC_VERSION__ >= 199901L)
static inline otel_log_value_t otel_log_value_empty(void) {
    otel_log_value_t value;
    value.value_type = OTEL_LOG_VALUE_TYPE_EMPTY;
    value.reserved = 0;
    value.value.string_value = otel_string_view_empty();
    return value;
}
static inline otel_log_value_t otel_log_value_string(otel_string_view_t s) {
    otel_log_value_t value;
    value.value_type = OTEL_LOG_VALUE_TYPE_STRING;
    value.reserved = 0;
    value.value.string_value = s;
    return value;
}
static inline otel_log_value_t otel_log_value_bool(otel_bool_t b) {
    otel_log_value_t value = otel_log_value_empty();
    value.value_type = OTEL_LOG_VALUE_TYPE_BOOL;
    value.value.bool_value = b;
    return value;
}
static inline otel_log_value_t otel_log_value_int64(int64_t i) {
    otel_log_value_t value = otel_log_value_empty();
    value.value_type = OTEL_LOG_VALUE_TYPE_INT64;
    value.value.int64_value = i;
    return value;
}
static inline otel_log_value_t otel_log_value_double(double d) {
    otel_log_value_t value = otel_log_value_empty();
    value.value_type = OTEL_LOG_VALUE_TYPE_DOUBLE;
    value.value.double_value = d;
    return value;
}
static inline otel_log_value_t otel_log_value_bytes(const uint8_t* ptr, size_t len) {
    otel_log_value_t value = otel_log_value_empty();
    value.value_type = OTEL_LOG_VALUE_TYPE_BYTES;
    value.value.bytes_value.ptr = ptr;
    value.value.bytes_value.len = len;
    return value;
}
/* Reference `count` array elements starting at node pool index `first`. */
static inline otel_log_value_t otel_log_value_array(uint32_t first, uint32_t count) {
    otel_log_value_t value = otel_log_value_empty();
    value.value_type = OTEL_LOG_VALUE_TYPE_ARRAY;
    value.value.children.first = first;
    value.value.children.count = count;
    return value;
}
/* Reference `count` map entries starting at node pool index `first`. */
static inline otel_log_value_t otel_log_value_map(uint32_t first, uint32_t count) {
    otel_log_value_t value = otel_log_value_empty();
    value.value_type = OTEL_LOG_VALUE_TYPE_MAP;
    value.value.children.first = first;
    value.value.children.count = count;
    return value;
}
static inline otel_log_key_value_t otel_log_kv(otel_string_view_t key,
                                               otel_log_value_t value) {
    otel_log_key_value_t kv;
    kv.key = key;
    kv.value = value;
    return kv;
}
/* An unkeyed node, for use as an array element in the node pool. */
static inline otel_log_key_value_t otel_log_element(otel_log_value_t value) {
    return otel_log_kv(otel_string_view_empty(), value);
}
#endif /* inline helpers */

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* OPENTELEMETRY_C_LOGS_H */
