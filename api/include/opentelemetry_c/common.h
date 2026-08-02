// SPDX-License-Identifier: Apache-2.0

/*
 * opentelemetry_c/common.h
 *
 * Common types shared across the OpenTelemetry C API: status codes, booleans,
 * string views, and typed key/value attributes, plus version and error queries.
 *
 * This header is part of the `opentelemetry-c` crate, a Rust-backed C binding for
 * OpenTelemetry. See README.md for status, ABI, and ownership rules.
 *
 * Thread-safety (summary; see sdk.h and trace.h for the full per-handle contract):
 *   - SDK, tracer-provider, and tracer handles may be used concurrently from multiple
 *     threads (every operation other than *_destroy takes a shared view internally).
 *   - A single span handle must NOT be used concurrently from multiple threads; use one
 *     span per thread or synchronize externally. Distinct spans are independent.
 *   - A builder handle is NOT thread-safe; confine it to a single thread.
 *   - No *_destroy may race with any other call on the same handle.
 *   - The Metrics API may invoke registered observable callbacks during collection. See
 *     metrics.h for callback concurrency and lifetime requirements.
 * Version and error queries are thread-safe; the last-error message is thread-local.
 *
 * Handle validity
 * ---------------
 * You must pass only NULL or a live handle of the exact expected type returned by this
 * library. Project handles share a fixed raw prefix that is checked before the complete
 * expected handle type is accessed, so a live handle of another OpenTelemetry C type is
 * rejected. This is defensive validation, NOT a general pointer-safety boundary: a foreign
 * pointer, freed/already-destroyed pointer, double destruction, or racing *_destroy with
 * another call remains undefined behavior (exactly like C `free`).
 *
 * Strings are passed as length-delimited UTF-8 views (`otel_string_view_t`) and are
 * copied by the library before it returns; the caller retains ownership of the
 * underlying bytes.
 */
#ifndef OPENTELEMETRY_C_COMMON_H
#define OPENTELEMETRY_C_COMMON_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Immutable, opaque trace-context snapshot shared in-process by Traces and Logs. */
typedef struct otel_span_context_t otel_span_context_t;

/*
 * Status code returned by fallible functions. OTEL_STATUS_OK (0) means success.
 * Any non-zero value indicates failure; call otel_last_error_message() for detail.
 *
 * New codes may be appended in future minor releases. Treat any unrecognized
 * non-zero value as a generic failure.
 */
typedef uint32_t otel_status_t;
enum {
    OTEL_STATUS_OK = 0,               /* Success. */
    OTEL_STATUS_INVALID_ARGUMENT = 1, /* NULL/invalid handle or malformed argument. */
    OTEL_STATUS_INVALID_UTF8 = 2,     /* A UTF-8 string argument was malformed. */
    OTEL_STATUS_INVALID_CONFIG = 3,   /* Readable config/ABI incompatible or unavailable. */
    OTEL_STATUS_ALREADY_SHUTDOWN = 4, /* The SDK/provider was already shut down. */
    OTEL_STATUS_TIMEOUT = 5,          /* Operation did not finish within the timeout. */
    OTEL_STATUS_EXPORT_FAILED = 6,    /* Exporter/callback pipeline failed (non-fatal). */
    OTEL_STATUS_INTERNAL_ERROR = 7    /* Wrapper/infrastructure failure or caught panic. */
};

/*
 * Status classification policy:
 *   - INVALID_ARGUMENT: an immediate argument is NULL, a project handle has the wrong
 *     type/state, an output pointer is invalid, or an immediate scalar/discriminant is
 *     malformed.
 *   - INVALID_CONFIG: readable configuration or one of its field values is invalid or
 *     incompatible, including an ABI kind/version/required-size mismatch or a requested
 *     feature/transport omitted from this build.
 *   - TIMEOUT: an operation with an enforced time bound exceeded that bound.
 *   - EXPORT_FAILED: an exporter or callback-driven export pipeline failed at runtime.
 *   - INTERNAL_ERROR: the wrapper/SDK infrastructure failed unexpectedly, including a
 *     caught panic, allocation failure, or worker-thread creation failure.
 *
 * Meaningful failures record a diagnostic for otel_last_error_message(). Diagnostics name
 * invalid fields or metadata keys where useful but do not include credential/header values.
 * Pointer readability and object lifetime remain caller obligations as documented above.
 */

/*
 * Boolean type. Crosses the ABI as a fixed-width uint32_t (not a C enum) so that any bit
 * pattern a caller passes is a well-defined value on the Rust side: 0 = false, any
 * non-zero value = true. Use the OTEL_FALSE / OTEL_TRUE constants below.
 */
typedef uint32_t otel_bool_t;
enum {
    OTEL_FALSE = 0,
    OTEL_TRUE = 1
};

/*
 * A borrowed, length-delimited UTF-8 string.
 *
 * The bytes need NOT be NUL-terminated. `ptr` may be NULL only when `len == 0`
 * (representing an empty/absent string). The referenced bytes must remain valid for
 * the duration of the call they are passed to; the library copies whatever it needs
 * to retain before returning.
 *
 * Construct one from a C string with otel_cstr() (see below) or by hand.
 */
typedef struct otel_string_view_t {
    const char* ptr; /* First UTF-8 byte, or NULL when len == 0. */
    size_t len;      /* Number of bytes. */
} otel_string_view_t;

/*
 * Discriminant selecting the active member of otel_attribute_value_t.
 *
 * Crosses the ABI as a fixed-width uint32_t (not a C enum): the Rust side validates it
 * before touching the union, so an out-of-range value is rejected (with
 * OTEL_STATUS_INVALID_ARGUMENT) rather than causing a type-confused read. Use the
 * OTEL_ATTRIBUTE_TYPE_* constants below.
 */
typedef uint32_t otel_attribute_type_t;
enum {
    OTEL_ATTRIBUTE_TYPE_STRING = 0,
    OTEL_ATTRIBUTE_TYPE_BOOL = 1,
    OTEL_ATTRIBUTE_TYPE_INT64 = 2,
    OTEL_ATTRIBUTE_TYPE_DOUBLE = 3
};

/* Tagged-union payload for an attribute value. Set the member matching the tag. */
typedef union otel_attribute_value_t {
    otel_string_view_t string_value; /* OTEL_ATTRIBUTE_TYPE_STRING */
    otel_bool_t bool_value;          /* OTEL_ATTRIBUTE_TYPE_BOOL   */
    int64_t int64_value;             /* OTEL_ATTRIBUTE_TYPE_INT64  */
    double double_value;             /* OTEL_ATTRIBUTE_TYPE_DOUBLE */
} otel_attribute_value_t;

/* A single typed attribute: a non-empty key plus a tagged value. */
typedef struct otel_key_value_t {
    otel_string_view_t key;          /* UTF-8 key; must not be empty. */
    otel_attribute_type_t value_type;/* Selects the active member of `value`.
                                        Values outside the OTEL_ATTRIBUTE_TYPE_* range
                                        are rejected with OTEL_STATUS_INVALID_ARGUMENT. */
    otel_attribute_value_t value;    /* The value payload. */
} otel_key_value_t;

/*
 * ABI layout guards (64-bit, C11+). These match compile-time assertions on the Rust
 * side; a failure means the header and library disagree about struct layout.
 */
#if defined(__STDC_VERSION__) && (__STDC_VERSION__ >= 201112L) && \
    defined(UINTPTR_MAX) && (UINTPTR_MAX == 0xFFFFFFFFFFFFFFFFu)
_Static_assert(sizeof(otel_string_view_t) == 16, "otel_string_view_t ABI mismatch");
_Static_assert(sizeof(otel_attribute_value_t) == 16, "otel_attribute_value_t ABI mismatch");
_Static_assert(sizeof(otel_key_value_t) == 40, "otel_key_value_t ABI mismatch");
#endif

/* ---- Version -------------------------------------------------------------- */

/* Major/minor/patch components of the library version. */
uint32_t otel_version_major(void);
uint32_t otel_version_minor(void);
uint32_t otel_version_patch(void);

/*
 * Full semantic version string (e.g. "0.1.0"). The returned view points at static
 * storage valid for the lifetime of the process; do not free it.
 */
otel_string_view_t otel_version_string(void);

/* ---- Errors --------------------------------------------------------------- */

/*
 * Retrieve the calling thread's last error message.
 *
 * Valid until the next OpenTelemetry C call on the same thread. If no error has been
 * recorded the returned view has a NULL `ptr` and zero `len`. The pointer is
 * NUL-terminated (so it may also be used as a C string), but `len` excludes the NUL.
 */
otel_string_view_t otel_last_error_message(void);

/* ---- Helpers -------------------------------------------------------------- */

#if defined(__cplusplus) || (defined(__STDC_VERSION__) && __STDC_VERSION__ >= 199901L)
#include <string.h>
/* Build a string view from a NUL-terminated C string. `s` may be NULL (=> empty). */
static inline otel_string_view_t otel_cstr(const char* s) {
    otel_string_view_t view;
    view.ptr = s;
    view.len = (s != NULL) ? strlen(s) : (size_t)0;
    return view;
}

/* An empty (absent) string view. */
static inline otel_string_view_t otel_string_view_empty(void) {
    otel_string_view_t view;
    view.ptr = NULL;
    view.len = 0;
    return view;
}

/*
 * Typed otel_key_value_t constructors.
 *
 * Each returns a POD attribute by value with the correct type tag and matching union member
 * set; no allocation, copy, or FFI call occurs. The `key` (and, for otel_kv_string, the
 * `value`) are BORROWED string views: the referenced bytes must remain valid until the
 * otel_span_* call the result is passed to returns, exactly as when filling an
 * otel_key_value_t by hand. Convenient for building attribute arrays for otel_span_add_event()
 * and for otel_span_set_attribute().
 */
static inline otel_key_value_t otel_kv_string(otel_string_view_t key, otel_string_view_t value) {
    otel_key_value_t kv;
    kv.key = key;
    kv.value_type = OTEL_ATTRIBUTE_TYPE_STRING;
    kv.value.string_value = value;
    return kv;
}
static inline otel_key_value_t otel_kv_bool(otel_string_view_t key, otel_bool_t value) {
    otel_key_value_t kv;
    kv.key = key;
    kv.value_type = OTEL_ATTRIBUTE_TYPE_BOOL;
    kv.value.bool_value = value;
    return kv;
}
static inline otel_key_value_t otel_kv_int64(otel_string_view_t key, int64_t value) {
    otel_key_value_t kv;
    kv.key = key;
    kv.value_type = OTEL_ATTRIBUTE_TYPE_INT64;
    kv.value.int64_value = value;
    return kv;
}
static inline otel_key_value_t otel_kv_double(otel_string_view_t key, double value) {
    otel_key_value_t kv;
    kv.key = key;
    kv.value_type = OTEL_ATTRIBUTE_TYPE_DOUBLE;
    kv.value.double_value = value;
    return kv;
}
#endif /* inline helpers */

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* OPENTELEMETRY_C_COMMON_H */
