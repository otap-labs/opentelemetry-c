/*
 * opentelemetry_c/span_processor.h
 *
 * The generic **span processor** handle (`otel_span_processor_t`) — the opaque object the SDK
 * builder consumes via otel_sdk_builder_add_span_processor(). A concrete processor is produced
 * by a processor constructor or builder (the simple span processor, see
 * simple_span_processor.h; and the batch span processor, see batch_span_processor.h). This
 * header is an opaque extension point: additional processor kinds can be added later behind the
 * same handle without reshaping the current C interface.
 *
 * Ownership: a span processor is owned by the caller until it is transferred into the SDK
 * builder via otel_sdk_builder_add_span_processor() (ownership moves on OTEL_STATUS_OK). If
 * never transferred, release it with otel_span_processor_destroy().
 *
 * Part of `libopentelemetry_c_sdk`. Requires linking the SDK alongside the API.
 */
#ifndef OPENTELEMETRY_C_SPAN_PROCESSOR_H
#define OPENTELEMETRY_C_SPAN_PROCESSOR_H

#ifdef __cplusplus
extern "C" {
#endif

/* Opaque span-processor handle. */
typedef struct otel_span_processor_t otel_span_processor_t;

/*
 * Destroy an untransferred span-processor handle (no-op on NULL). A successful transfer into
 * the SDK builder consumes the handle and invalidates the original pointer; never access or
 * destroy that pointer afterward. Must not race with any other use of the same handle.
 */
void otel_span_processor_destroy(otel_span_processor_t* processor);

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* OPENTELEMETRY_C_SPAN_PROCESSOR_H */
