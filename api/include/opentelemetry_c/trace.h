/*
 * opentelemetry_c/trace.h
 *
 * Trace API: tracer providers, tracers, and spans, exposed as opaque handles.
 *
 * Handle ownership
 * ----------------
 * Every function that returns a handle transfers ownership to the caller, who must
 * release it with the matching *_destroy function. Passing NULL to any *_destroy is a
 * no-op. Handles must not be used after they are destroyed.
 *
 * Thread-safety
 * -------------
 * Providers and tracers are safe to share and use across threads. A single span handle
 * must NOT be used concurrently from multiple threads; use one span per thread (or
 * external synchronization). Distinct spans may be used concurrently. No *_destroy may
 * race with any other call on the same handle.
 */
#ifndef OPENTELEMETRY_C_TRACE_H
#define OPENTELEMETRY_C_TRACE_H

#include "common.h"

#ifdef __cplusplus
extern "C" {
#endif

/* Opaque handles. */
typedef struct otel_tracer_provider_t otel_tracer_provider_t;
typedef struct otel_tracer_t otel_tracer_t;
typedef struct otel_span_t otel_span_t;

/*
 * Span kind, mirroring the OpenTelemetry specification.
 *
 * Crosses the ABI as a fixed-width uint32_t (not a C enum). A value outside the range
 * below is treated as OTEL_SPAN_KIND_INTERNAL rather than producing an invalid value on
 * the Rust side. Use the OTEL_SPAN_KIND_* constants below.
 */
typedef uint32_t otel_span_kind_t;
enum {
    OTEL_SPAN_KIND_INTERNAL = 0, /* Default. */
    OTEL_SPAN_KIND_SERVER = 1,
    OTEL_SPAN_KIND_CLIENT = 2,
    OTEL_SPAN_KIND_PRODUCER = 3,
    OTEL_SPAN_KIND_CONSUMER = 4
};

/*
 * Span status code.
 *
 * Crosses the ABI as a fixed-width uint32_t (not a C enum). A value outside the range
 * below is rejected by otel_span_set_status() with OTEL_STATUS_INVALID_ARGUMENT. Use the
 * OTEL_SPAN_STATUS_* constants below.
 */
typedef uint32_t otel_span_status_code_t;
enum {
    OTEL_SPAN_STATUS_UNSET = 0, /* Default. */
    OTEL_SPAN_STATUS_OK = 1,
    OTEL_SPAN_STATUS_ERROR = 2
};

/*
 * Options for otel_tracer_start_span(). A NULL options pointer selects
 * OTEL_SPAN_KIND_INTERNAL and no explicit parent (a new root span).
 */
typedef struct otel_span_start_options_t {
    otel_span_kind_t kind;       /* The span kind. Unknown values fall back to
                                    OTEL_SPAN_KIND_INTERNAL. */
    const otel_span_t* parent;   /* Optional parent span; NULL => root span. */
} otel_span_start_options_t;

#if defined(__STDC_VERSION__) && (__STDC_VERSION__ >= 201112L) && \
    defined(UINTPTR_MAX) && (UINTPTR_MAX == 0xFFFFFFFFFFFFFFFFu)
_Static_assert(sizeof(otel_span_start_options_t) == 16,
               "otel_span_start_options_t ABI mismatch");
#endif

/* ---- Provider ------------------------------------------------------------- */

/*
 * Return an owned handle to the process-global tracer provider. Never NULL under
 * normal conditions. Release with otel_tracer_provider_destroy(). Tracers obtained
 * from it reflect whichever SDK is installed as global at the time of the request.
 */
otel_tracer_provider_t* otel_global_tracer_provider(void);

/*
 * Obtain a tracer from a provider.
 *
 *   name       - instrumentation scope name (required, non-empty recommended).
 *   version    - instrumentation scope version; pass an empty view to omit.
 *   schema_url - instrumentation schema URL; pass an empty view to omit.
 *
 * Return value:
 *   - Invalid provider handle: NULL.
 *   - No SDK installed (unbacked global provider): a valid no-op tracer.
 *   - A backed implementation whose tracer creation fails (e.g. a malformed string view or
 *     allocation failure): NULL, with the last-error set (see otel_last_error_message()) —
 *     NOT a no-op tracer.
 * Release with otel_tracer_destroy().
 */
otel_tracer_t* otel_tracer_provider_get_tracer(const otel_tracer_provider_t* provider,
                                               otel_string_view_t name,
                                               otel_string_view_t version,
                                               otel_string_view_t schema_url);

/*
 * Destroy a tracer-provider handle (no-op on NULL). Does NOT shut down the underlying
 * SDK; use otel_sdk_shutdown() for that.
 */
void otel_tracer_provider_destroy(otel_tracer_provider_t* provider);

/* ---- Tracer --------------------------------------------------------------- */

/*
 * Start a new span.
 *
 *   name    - span name (required).
 *   options - optional; NULL => internal-kind root span.
 *
 * Parenting: if options->parent is non-NULL it must be a live span handle. A parent span
 * produced by a DIFFERENT implementation (i.e. created via a different tracer/vtable than
 * this tracer) is treated as NO parent, so the new span becomes a root span.
 *
 * Return value:
 *   - Invalid tracer handle, or a non-NULL but invalid parent handle: NULL.
 *   - Unbacked (no-op) tracer: a valid no-op span.
 *   - A backed tracer whose span creation fails (e.g. a malformed name): NULL, with the
 *     last-error set — NOT a no-op span.
 * The returned span must be ended with otel_span_end() and released with
 * otel_span_destroy(). Destroying a span that was not explicitly ended performs a
 * best-effort end first.
 */
otel_span_t* otel_tracer_start_span(const otel_tracer_t* tracer,
                                    otel_string_view_t name,
                                    const otel_span_start_options_t* options);

/*
 * Start a span from an immutable, implementation-neutral parent context. `parent` is borrowed
 * for the call. If options->parent is non-NULL the call fails: a live parent span and a context
 * snapshot are mutually exclusive. An older installed SDK that lacks this operation returns
 * NULL. With no SDK installed, the call returns a valid no-op span.
 */
otel_span_t* otel_tracer_start_span_with_context(
    const otel_tracer_t* tracer,
    otel_string_view_t name,
    const otel_span_start_options_t* options,
    const otel_span_context_t* parent);

/* ---- Extended span start (links, explicit start time, attributes) --------- */

/*
 * A span link: an immutable parent context plus optional link attributes. All pointers are
 * borrowed for the duration of the otel_tracer_start_span_ex() call only.
 */
typedef struct otel_span_link_t {
    const otel_span_context_t* context;  /* Linked context; must be a live, valid handle. */
    const otel_key_value_t* attributes;  /* Optional; NULL when attribute_count == 0. */
    size_t attribute_count;
} otel_span_link_t;

/*
 * Versioned options for otel_tracer_start_span_ex().
 *
 * The first field, struct_size, MUST be set to sizeof(otel_span_start_options_ex_t) as the
 * caller compiled it. The implementation reads only the fields covered by struct_size, so an
 * older caller and a newer library (or vice versa) interoperate: fields beyond a caller's
 * struct_size are ignored, and struct_size must cover at least through start_time_unix_nanos.
 *
 * parent and parent_context are mutually exclusive (at most one may be non-NULL). A live parent
 * produced by a DIFFERENT implementation is treated as NO parent (root span), matching
 * otel_tracer_start_span().
 */
typedef struct otel_span_start_options_ex_t {
    size_t struct_size;                        /* = sizeof(otel_span_start_options_ex_t). */
    otel_span_kind_t kind;                     /* Span kind; unknown => INTERNAL. */
    uint32_t reserved;                         /* Must be 0. */
    const otel_span_t* parent;                 /* Optional live parent; NULL => none. */
    const otel_span_context_t* parent_context; /* Optional context parent; NULL => none. */
    uint64_t start_time_unix_nanos;            /* 0 => unset (implementation assigns now). */
    const otel_key_value_t* attributes;        /* Optional initial attributes; NULL if count 0. */
    size_t attribute_count;
    const otel_span_link_t* links;             /* Optional links; NULL when link_count == 0. */
    size_t link_count;
} otel_span_start_options_ex_t;

/* Initializer setting struct_size and zeroing every other field. */
#define OTEL_SPAN_START_OPTIONS_EX_INIT \
    { sizeof(otel_span_start_options_ex_t), OTEL_SPAN_KIND_INTERNAL, 0, NULL, NULL, 0, NULL, 0, NULL, 0 }

#if defined(__STDC_VERSION__) && (__STDC_VERSION__ >= 201112L) && \
    defined(UINTPTR_MAX) && (UINTPTR_MAX == 0xFFFFFFFFFFFFFFFFu)
_Static_assert(sizeof(otel_span_start_options_ex_t) == 72,
               "otel_span_start_options_ex_t ABI mismatch");
#endif

/*
 * Start a span from a versioned descriptor supporting links, an explicit start timestamp, and
 * initial attributes. `options` must be non-NULL with struct_size set as described above.
 *
 * Return value:
 *   - Invalid tracer, NULL options, struct_size too small, non-zero reserved, both parents set,
 *     or an invalid parent/link context: NULL, with the last-error set.
 *   - Unbacked (no-op) tracer: a valid no-op span.
 *   - A backed tracer whose installed SDK predates this operation: NULL with
 *     OTEL_STATUS_INVALID_CONFIG.
 * The returned span must be ended with otel_span_end() and released with otel_span_destroy().
 */
otel_span_t* otel_tracer_start_span_ex(const otel_tracer_t* tracer,
                                       otel_string_view_t name,
                                       const otel_span_start_options_ex_t* options);

/* Destroy a tracer handle (no-op on NULL). */
void otel_tracer_destroy(otel_tracer_t* tracer);

/* ---- Span ----------------------------------------------------------------- */

/*
 * Set a typed attribute. Keys must be non-empty UTF-8. SDK-backed spans reject invalid keys
 * or string values; no-SDK no-op spans may skip validation because the call records nothing.
 */
otel_status_t otel_span_set_string_attribute(otel_span_t* span,
                                             otel_string_view_t key,
                                             otel_string_view_t value);
otel_status_t otel_span_set_bool_attribute(otel_span_t* span,
                                           otel_string_view_t key,
                                           otel_bool_t value);
otel_status_t otel_span_set_int64_attribute(otel_span_t* span,
                                            otel_string_view_t key,
                                            int64_t value);
otel_status_t otel_span_set_double_attribute(otel_span_t* span,
                                             otel_string_view_t key,
                                             double value);

/*
 * Set an attribute from a tagged key/value. SDK-backed spans reject invalid tags, keys, or
 * string values; no-SDK no-op spans may skip validation because the call records nothing.
 */
otel_status_t otel_span_set_attribute(otel_span_t* span, otel_key_value_t attribute);

/*
 * Add a timestamped event with optional attributes. `attributes` may be NULL when
 * `attribute_count` is 0.
 */
otel_status_t otel_span_add_event(otel_span_t* span,
                                  otel_string_view_t name,
                                  const otel_key_value_t* attributes,
                                  size_t attribute_count);

/*
 * Set the span status. For OTEL_SPAN_STATUS_ERROR, `description` carries the error
 * message; for other codes it is ignored and may be an empty view. SDK-backed spans reject
 * a `code` outside otel_span_status_code_t with OTEL_STATUS_INVALID_ARGUMENT; no-SDK no-op
 * spans may skip validation because the call records nothing.
 */
otel_status_t otel_span_set_status(otel_span_t* span,
                                   otel_span_status_code_t code,
                                   otel_string_view_t description);

/* Rename a span. */
otel_status_t otel_span_update_name(otel_span_t* span, otel_string_view_t name);

/*
 * End a span, recording its end timestamp. Idempotent: calling more than once is safe
 * and returns OTEL_STATUS_OK without re-ending.
 */
otel_status_t otel_span_end(otel_span_t* span);

/*
 * Destroy a span handle (no-op on NULL). If the span was not explicitly ended, this
 * performs a best-effort end first.
 */
void otel_span_destroy(otel_span_t* span);

/* ---- Immutable SpanContext snapshots ------------------------------------- */

/*
 * Copy the immutable context of a live SDK-backed span into an API-owned handle. A no-op span
 * has no valid context and returns OTEL_STATUS_INVALID_CONFIG. * The snapshot remains valid after
 * the source span ends or is destroyed and is safe to share across threads. Snapshots are
 * opaque and in-process; see the "W3C Trace Context propagation" section below for the
 * traceparent/tracestate import/export APIs.
 */
otel_status_t otel_span_get_context(const otel_span_t* span, otel_span_context_t** out);

/* Return a new independent handle with the same immutable contents, or NULL on failure. */
otel_span_context_t* otel_span_context_clone(const otel_span_context_t* context);

/* Release one owned snapshot handle. NULL is accepted. */
void otel_span_context_destroy(otel_span_context_t* context);

/* ---- SpanContext value access and construction ---------------------------- */

/*
 * Accessors over an immutable, API-owned otel_span_context_t. See TRACES_COMPLIANCE.md
 * ("SpanContext value API") for the full contract. All are safe on any live context handle
 * and are thread-safe (the context is immutable). A NULL or wrong-kind handle yields the
 * documented empty/false/INVALID_ARGUMENT result without dereferencing memory it does not own.
 *
 * Byte order: trace and span IDs are big-endian (W3C/network order) — the same order used by
 * the traceparent hex encoding. Trace flags are an opaque uint8_t; all 8 bits are preserved
 * (only the sampled bit 0x01 has defined meaning today, unknown bits are retained).
 */

/* Whether the context is valid (non-zero trace ID and span ID). NULL/invalid => OTEL_FALSE. */
otel_bool_t otel_span_context_is_valid(const otel_span_context_t* context);

/* Whether the context was extracted from a remote parent. NULL/invalid => OTEL_FALSE. */
otel_bool_t otel_span_context_is_remote(const otel_span_context_t* context);

/* Copy the 16-byte trace ID into `out` (>= 16 writable bytes). */
otel_status_t otel_span_context_trace_id(const otel_span_context_t* context, uint8_t out[16]);

/* Copy the 8-byte span ID into `out` (>= 8 writable bytes). */
otel_status_t otel_span_context_span_id(const otel_span_context_t* context, uint8_t out[8]);

/* Write the opaque trace flags into `*out`. */
otel_status_t otel_span_context_trace_flags(const otel_span_context_t* context, uint8_t* out);

/*
 * Borrow the tracestate as a UTF-8 view. The bytes are owned by `context` and valid until it
 * is destroyed; copy them to retain longer. Empty tracestate or a NULL/wrong-kind handle
 * yields an empty view (ptr == NULL, len == 0). The view is NOT NUL-terminated.
 */
otel_string_view_t otel_span_context_tracestate(const otel_span_context_t* context);

/*
 * Construct an owned immutable span context from raw parts. `trace_id` points to 16 bytes and
 * `span_id` to 8 bytes, both big-endian. `trace_flags` is stored opaquely. `trace_state` is a
 * borrowed UTF-8 view copied before return (empty view => none). All-zero trace/span IDs are
 * rejected. Returns NULL on invalid arguments or allocation failure, with the last-error set.
 * Release with otel_span_context_destroy().
 */
otel_span_context_t* otel_span_context_create(const uint8_t trace_id[16],
                                              const uint8_t span_id[8],
                                              uint8_t trace_flags,
                                              otel_bool_t is_remote,
                                              otel_string_view_t trace_state);

/* ---- W3C Trace Context propagation ---------------------------------------- */

/*
 * A bounded, direct traceparent/tracestate propagation API. See TRACES_COMPLIANCE.md
 * ("W3C Trace Context propagation"). No borrowed pointer or callback state is retained past
 * a call; all input sizes are bounded before allocation. Baggage is out of scope.
 */

/*
 * Extract a remote span context from a W3C `traceparent` and optional `tracestate`.
 *
 *   traceparent - required header value (e.g. "00-<32hex>-<16hex>-<2hex>").
 *   tracestate  - optional; pass an empty view for none.
 *   out         - receives a new owned context with is_remote == true, or NULL on failure.
 *
 * Malformed length/separators/IDs, an all-zero trace or span ID, version "ff", forbidden
 * trailing data, or an invalid tracestate are rejected with OTEL_STATUS_INVALID_ARGUMENT and
 * *out == NULL. Unknown/reserved trace-flag bits are preserved. Release *out with
 * otel_span_context_destroy().
 */
otel_status_t otel_trace_propagation_extract(otel_string_view_t traceparent,
                                             otel_string_view_t tracestate,
                                             otel_span_context_t** out);

/*
 * Format the `traceparent` for `context` into `buffer` (not NUL-terminated).
 *
 * Length/query contract shared by both injectors:
 *   - If `out_len` is non-NULL it always receives the exact required byte length.
 *   - If `buffer` is NULL the call is a pure length query and returns OTEL_STATUS_OK.
 *   - If `buffer` is non-NULL and `capacity` >= required, the bytes are written and OK is
 *     returned; otherwise OTEL_STATUS_INVALID_ARGUMENT is returned with `out_len` still set,
 *     so the caller can resize and retry.
 * An invalid context returns OTEL_STATUS_INVALID_ARGUMENT. A version-00 traceparent is 55
 * bytes.
 */
otel_status_t otel_trace_propagation_inject_traceparent(const otel_span_context_t* context,
                                                        char* buffer,
                                                        size_t capacity,
                                                        size_t* out_len);

/*
 * Format the `tracestate` for `context` into `buffer` (not NUL-terminated). Uses the same
 * length/query contract as otel_trace_propagation_inject_traceparent(). An empty tracestate
 * yields *out_len == 0 and writes nothing.
 */
otel_status_t otel_trace_propagation_inject_tracestate(const otel_span_context_t* context,
                                                       char* buffer,
                                                       size_t capacity,
                                                       size_t* out_len);

/* ---- Convenience helpers -------------------------------------------------- */

#if defined(__cplusplus) || (defined(__STDC_VERSION__) && __STDC_VERSION__ >= 199901L)
/*
 * Optional header-only shorthands over otel_span_set_status(). Each performs exactly one FFI
 * call — the same otel_span_set_status() the caller would make — and returns its status
 * unchanged; no allocation or copy is added. otel_span_set_ok() passes an empty description
 * (ignored for non-error codes). For otel_span_set_error() the `description` bytes are
 * BORROWED and must remain valid until the call returns.
 */
static inline otel_status_t otel_span_set_ok(otel_span_t* span) {
    return otel_span_set_status(span, OTEL_SPAN_STATUS_OK, otel_string_view_empty());
}
static inline otel_status_t otel_span_set_error(otel_span_t* span,
                                                otel_string_view_t description) {
    return otel_span_set_status(span, OTEL_SPAN_STATUS_ERROR, description);
}
#endif /* inline helpers */

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* OPENTELEMETRY_C_TRACE_H */
