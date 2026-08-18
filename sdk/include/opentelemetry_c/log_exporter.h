// SPDX-License-Identifier: Apache-2.0

/*
 * opentelemetry_c/log_exporter.h
 *
 * The generic **log exporter** handle (`otel_log_exporter_t`) — the opaque object a log
 * processor consumes. Concrete exporters are produced by exporter builders (today only OTLP;
 * see otlp_log_exporter.h). This header is an opaque extension point: further exporter kinds
 * can be added later without reshaping the current C interface.
 *
 * EXPERIMENTAL: the Logs C API is experimental and may change in a future release.
 *
 * Ownership: an exporter is owned by the caller until it is transferred into a log processor
 * (otel_simple_log_processor_create() or otel_batch_log_processor_builder_set_exporter()),
 * where ownership moves on OTEL_STATUS_OK. If never transferred, release it with
 * otel_log_exporter_destroy().
 *
 * Part of `libopentelemetry_c_sdk`. Requires linking the SDK alongside the API.
 */
#ifndef OPENTELEMETRY_C_LOG_EXPORTER_H
#define OPENTELEMETRY_C_LOG_EXPORTER_H

#ifdef __cplusplus
extern "C" {
#endif

/* Opaque log-exporter handle. */
typedef struct otel_log_exporter_t otel_log_exporter_t;

/*
 * Destroy an untransferred log-exporter handle (no-op on NULL). A successful transfer into a
 * log processor consumes the handle and invalidates the original pointer; never access or
 * destroy that pointer afterward. Must not race with any other use of the same handle.
 */
void otel_log_exporter_destroy(otel_log_exporter_t* exporter);

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* OPENTELEMETRY_C_LOG_EXPORTER_H */
