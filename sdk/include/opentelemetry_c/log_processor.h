/*
 * opentelemetry_c/log_processor.h
 *
 * The generic **log processor** handle (`otel_log_processor_t`) — the opaque object the SDK
 * builder consumes via otel_sdk_builder_add_log_processor() — plus the two processor
 * constructors this SDK provides:
 *
 *   - simple: exports each record synchronously, on the thread that called
 *     otel_logger_emit(). Intended for tests and low-volume diagnostics.
 *   - batch:  queues records and exports them from a dedicated SDK-owned OS thread.
 *     Intended for production.
 *
 * EXPERIMENTAL: the Logs C API is experimental and may change in a future release.
 *
 * Ownership: a processor is owned by the caller until it is transferred into the SDK builder
 * via otel_sdk_builder_add_log_processor() (ownership moves on OTEL_STATUS_OK). If never
 * transferred, release it with otel_log_processor_destroy().
 *
 * Part of `libopentelemetry_c_sdk`. Requires linking the SDK alongside the API.
 */
#ifndef OPENTELEMETRY_C_LOG_PROCESSOR_H
#define OPENTELEMETRY_C_LOG_PROCESSOR_H

#include <opentelemetry_c/common.h>
#include <opentelemetry_c/log_exporter.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Opaque handles. */
typedef struct otel_log_processor_t otel_log_processor_t;
typedef struct otel_batch_log_processor_builder_t otel_batch_log_processor_builder_t;

/*
 * Destroy an untransferred log-processor handle (no-op on NULL). A successful transfer into
 * the SDK builder consumes the handle and invalidates the original pointer. Must not race
 * with any other use of the same handle. Destroying an untransferred batch processor makes
 * its worker exit but does not perform a draining shutdown: queued records may be discarded
 * and exporter shutdown is not guaranteed. Transfer production processors into an SDK and
 * use otel_sdk_logs_shutdown() for a draining stop.
 */
void otel_log_processor_destroy(otel_log_processor_t* processor);

/*
 * Create a simple log processor that exports each record on the emitting thread before
 * otel_logger_emit() returns.
 *
 * Ownership: on OTEL_STATUS_OK `exporter` is consumed and its pointer becomes invalid, and
 * *out receives a new processor handle. On failure *out is set to NULL and the caller still
 * owns `exporter`.
 */
otel_status_t otel_simple_log_processor_create(otel_log_exporter_t* exporter,
                                               otel_log_processor_t** out);

/* ---- Batch log processor -------------------------------------------------- */

/* Create a batch log-processor builder with spec-default settings. NULL only on allocation
 * failure. A builder is NOT thread-safe; confine it to a single thread. Release with
 * otel_batch_log_processor_builder_destroy(). */
otel_batch_log_processor_builder_t* otel_batch_log_processor_builder_new(void);

/* Destroy a batch log-processor builder (no-op on NULL). Frees an exporter that was
 * transferred to the builder but not yet consumed by a successful build. */
void otel_batch_log_processor_builder_destroy(otel_batch_log_processor_builder_t* builder);

/*
 * Transfer the exporter this processor exports through. On OTEL_STATUS_OK ownership of
 * `exporter` moves into the builder and the original pointer becomes invalid; on failure the
 * caller still owns it. Setting a second exporter replaces and releases the first.
 */
otel_status_t otel_batch_log_processor_builder_set_exporter(
    otel_batch_log_processor_builder_t* builder, otel_log_exporter_t* exporter);

/* Maximum number of records buffered before new records are dropped (0 == SDK default). */
otel_status_t otel_batch_log_processor_builder_set_max_queue_size(
    otel_batch_log_processor_builder_t* builder, size_t max_queue_size);

/* Maximum records exported per batch (0 == SDK default). Must not exceed the queue size when
 * both are set explicitly, or build returns OTEL_STATUS_INVALID_CONFIG. */
otel_status_t otel_batch_log_processor_builder_set_max_export_batch_size(
    otel_batch_log_processor_builder_t* builder, size_t max_export_batch_size);

/* Delay between scheduled exports, in milliseconds (0 == SDK default). */
otel_status_t otel_batch_log_processor_builder_set_scheduled_delay_millis(
    otel_batch_log_processor_builder_t* builder, uint64_t scheduled_delay_millis);

/*
 * NOTE: there is deliberately no per-export timeout setter here, although the Trace batch
 * processor has one. The pinned upstream Rust Logs batch configuration exposes no such knob,
 * so this API would have to accept a value it could never apply. Rather than return OK for
 * configuration that does nothing, the entry point is omitted; it can be added compatibly
 * once upstream supports it. The synchronous batch processor applies no separate per-export
 * deadline and does not read OTEL_BLRP_EXPORT_TIMEOUT; configure the OTLP transport timeout
 * with otel_otlp_log_exporter_builder_set_timeout_millis().
 */

/*
 * Build a batch log processor. Requires an exporter. On OTEL_STATUS_OK *out receives a new
 * processor handle owned by the caller, and the exporter transferred earlier moves into it —
 * so a second build on the same builder fails with OTEL_STATUS_INVALID_CONFIG rather than
 * reusing a consumed exporter. The builder itself remains owned by the caller.
 *
 * Building starts the processor's worker OS thread; a failure to start is reported as
 * OTEL_STATUS_INTERNAL_ERROR.
 */
otel_status_t otel_batch_log_processor_builder_build(
    otel_batch_log_processor_builder_t* builder, otel_log_processor_t** out);

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* OPENTELEMETRY_C_LOG_PROCESSOR_H */
