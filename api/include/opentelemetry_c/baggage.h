/*
 * opentelemetry_c/baggage.h
 *
 * Immutable, API-owned OpenTelemetry Baggage and bounded W3C propagation.
 */
#ifndef OPENTELEMETRY_C_BAGGAGE_H
#define OPENTELEMETRY_C_BAGGAGE_H

#include "common.h"

#ifdef __cplusplus
extern "C" {
#endif

typedef struct otel_baggage_t otel_baggage_t;
typedef struct otel_baggage_builder_t otel_baggage_builder_t;

typedef struct otel_baggage_entry_view_t {
    size_t struct_size;
    otel_string_view_t key;
    otel_string_view_t value;
    otel_string_view_t metadata;
} otel_baggage_entry_view_t;

#define OTEL_BAGGAGE_ENTRY_VIEW_INIT \
    { sizeof(otel_baggage_entry_view_t), {NULL, 0}, {NULL, 0}, {NULL, 0} }

#if defined(__STDC_VERSION__) && (__STDC_VERSION__ >= 201112L) && \
    defined(UINTPTR_MAX) && (UINTPTR_MAX == 0xFFFFFFFFFFFFFFFFu)
_Static_assert(sizeof(otel_baggage_entry_view_t) == 56,
               "otel_baggage_entry_view_t ABI mismatch");
#endif

typedef otel_status_t (*otel_baggage_visitor_t)(void* user_data,
                                                const otel_baggage_entry_view_t* entry);

/* Create a mutable builder. The builder remains caller-owned after build, whether build
 * succeeds or fails, and must be released with otel_baggage_builder_destroy(). */
otel_baggage_builder_t* otel_baggage_builder_create(void);

/* Set or replace a case-sensitive entry. Key, value, and metadata are copied. Keys must be
 * non-empty UTF-8; values and metadata accept all valid UTF-8, including embedded NUL bytes.
 * This implementation bounds logical baggage to 64 entries and 8192 stored bytes. These are
 * implementation resource limits, not a claimed W3C per-member limit. */
otel_status_t otel_baggage_builder_set(otel_baggage_builder_t* builder,
                                       otel_string_view_t key,
                                       otel_string_view_t value,
                                       otel_string_view_t metadata);
otel_status_t otel_baggage_builder_remove(otel_baggage_builder_t* builder,
                                          otel_string_view_t key);
otel_status_t otel_baggage_builder_build(const otel_baggage_builder_t* builder,
                                         otel_baggage_t** out);
void otel_baggage_builder_destroy(otel_baggage_builder_t* builder);

otel_baggage_t* otel_baggage_clone(const otel_baggage_t* baggage);
void otel_baggage_destroy(otel_baggage_t* baggage);
size_t otel_baggage_count(const otel_baggage_t* baggage);

/* Initialize out with OTEL_BAGGAGE_ENTRY_VIEW_INIT. Return OTEL_TRUE and fill it when key
 * exists, or OTEL_FALSE when it does not. The returned
 * views borrow immutable storage owned by baggage and remain valid until baggage is destroyed.
 * They are length-delimited and are not guaranteed to be NUL-terminated. */
otel_bool_t otel_baggage_get(const otel_baggage_t* baggage,
                             otel_string_view_t key,
                             otel_baggage_entry_view_t* out);

/* Visit each entry synchronously in unspecified order. Entry views are valid only for the
 * callback. A non-OK visitor result stops traversal and is returned unchanged. */
otel_status_t otel_baggage_visit(const otel_baggage_t* baggage,
                                 otel_baggage_visitor_t visitor,
                                 void* user_data);

/* Parse a W3C baggage header. Malformed members are skipped independently, later duplicate
 * keys replace earlier ones, and an input larger than 8192 bytes yields valid empty baggage.
 * Remote malformed baggage is therefore isolated from Trace Context extraction. */
otel_status_t otel_baggage_propagation_extract(otel_string_view_t header,
                                               otel_baggage_t** out);

/* Encode W3C baggage, omitting complete entries that cannot be represented or fitted within
 * the 64-member/8192-byte propagation policy. No partial member is emitted. A NULL buffer with
 * capacity 0 is a successful length query. out_len excludes any NUL terminator; none is added. */
otel_status_t otel_baggage_propagation_inject(const otel_baggage_t* baggage,
                                              char* buffer,
                                              size_t capacity,
                                              size_t* out_len);

#ifdef __cplusplus
}
#endif

#endif /* OPENTELEMETRY_C_BAGGAGE_H */
