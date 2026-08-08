// SPDX-License-Identifier: Apache-2.0

/*
 * opentelemetry_c/simple_span_processor.h
 *
 * The **simple span processor** constructor. It consumes a trace exporter (see
 * trace_exporter.h) and produces a generic otel_span_processor_t (see span_processor.h) that
 * the SDK builder then consumes.
 *
 * Unlike the batch span processor, a simple processor has no configuration and no worker
 * thread: it exports each finished span synchronously, on the thread that ended the span,
 * serializing ending threads behind one exporter. It is intended for tests, short-lived
 * programs, and debugging; production pipelines should prefer the batch span processor
 * (batch_span_processor.h).
 *
 * Part of `libopentelemetry_c_sdk`. Requires linking the SDK alongside the API.
 */
#ifndef OPENTELEMETRY_C_SIMPLE_SPAN_PROCESSOR_H
#define OPENTELEMETRY_C_SIMPLE_SPAN_PROCESSOR_H

#include <opentelemetry_c/common.h>
#include <opentelemetry_c/span_processor.h>
#include <opentelemetry_c/trace_exporter.h>

#ifdef __cplusplus
extern "C" {
#endif

/*
 * Create a simple span processor that exports each finished span on the thread that ended it,
 * before that thread continues.
 *
 * Ownership: on OTEL_STATUS_OK `exporter` is consumed and its pointer becomes invalid, and
 * *out receives a new processor handle owned by the caller (release with
 * otel_span_processor_destroy(), or transfer it into the SDK builder via
 * otel_sdk_builder_add_span_processor()). On failure *out is set to NULL and the caller still
 * owns `exporter`.
 */
otel_status_t otel_simple_span_processor_create(otel_trace_exporter_t* exporter,
                                                otel_span_processor_t** out);

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* OPENTELEMETRY_C_SIMPLE_SPAN_PROCESSOR_H */
