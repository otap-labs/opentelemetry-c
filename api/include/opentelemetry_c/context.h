/*
 * opentelemetry_c/context.h
 *
 * API-owned immutable context and thread-local attachment scopes. This context is independent
 * of opentelemetry-rust's ambient Context; explicit SDK conversion occurs at the ABI boundary.
 */
#ifndef OPENTELEMETRY_C_CONTEXT_H
#define OPENTELEMETRY_C_CONTEXT_H

#include "trace.h"

#ifdef __cplusplus
extern "C" {
#endif

typedef struct otel_context_t otel_context_t;

/* Caller-owned, non-owning attachment token. Initialize before every attach. Do not modify it
 * while active. Copies are harmless but only the first matching LIFO detach can succeed. */
typedef struct otel_context_scope_t {
    size_t struct_size;
    uint64_t thread_token;
    uint64_t generation;
    uint64_t reserved[2];
} otel_context_scope_t;

#define OTEL_CONTEXT_SCOPE_INIT { sizeof(otel_context_scope_t), 0, 0, {0, 0} }

#if defined(__STDC_VERSION__) && (__STDC_VERSION__ >= 201112L) && \
    defined(UINTPTR_MAX) && (UINTPTR_MAX == 0xFFFFFFFFFFFFFFFFu)
_Static_assert(sizeof(otel_context_scope_t) == 40, "otel_context_scope_t ABI mismatch");
#endif

/* Create an immutable context. NULL span_context creates an explicitly empty/root context. */
otel_context_t* otel_context_create(const otel_span_context_t* span_context);
otel_context_t* otel_context_clone(const otel_context_t* context);
void otel_context_destroy(otel_context_t* context);

/* Return an owned snapshot of this thread's current context, or an empty context if none is
 * attached. Release it with otel_context_destroy(). */
otel_context_t* otel_context_current(void);

/* Return a new owned SpanContext handle, or NULL when this context is empty. */
otel_span_context_t* otel_context_span_context(const otel_context_t* context);

/* Attach context on the current thread. On success the TLS stack owns a retained context until
 * the matching detach. At most 64 contexts may be nested. When scope has a compatible
 * struct_size, every failed attach leaves it inactive and detach returns a safe defined error. */
otel_status_t otel_context_attach(const otel_context_t* context, otel_context_scope_t* scope);

/* Detach exactly once, on the attaching thread, in LIFO order. Wrong-thread, copied-token,
 * inactive, and out-of-order detaches fail without changing the TLS stack. */
otel_status_t otel_context_scope_detach(otel_context_scope_t* scope);

#ifdef __cplusplus
}
#endif

#endif /* OPENTELEMETRY_C_CONTEXT_H */
