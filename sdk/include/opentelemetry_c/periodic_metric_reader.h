#ifndef OPENTELEMETRY_C_PERIODIC_METRIC_READER_H
#define OPENTELEMETRY_C_PERIODIC_METRIC_READER_H

#include <opentelemetry_c/common.h>
#include <opentelemetry_c/metric_exporter.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct otel_periodic_metric_reader_builder_t otel_periodic_metric_reader_builder_t;
typedef struct otel_periodic_metric_reader_t otel_periodic_metric_reader_t;

typedef uint32_t otel_metric_reader_runtime_t;
enum {
    OTEL_METRIC_READER_RUNTIME_BLOCKING = 0,
    OTEL_METRIC_READER_RUNTIME_ASYNC = 1
};

/*
 * The pinned OpenTelemetry Rust 0.32 PeriodicReader uses a fixed five-second exporter
 * timeout while shutting down. otel_sdk_metrics_shutdown() cannot override that reader
 * timeout; its timeout_millis argument is currently advisory for Metrics. The optional async
 * reader instead passes its configured export timeout to interval and force-flush exports.
 * This is a cooperative async timeout: it cannot interrupt a synchronous exporter call. The
 * custom C callbacks execute synchronously, so callers must ensure those callbacks return
 * promptly. Both runtimes are SDK-owned; callers never provide or enter a Rust runtime.
 */
otel_periodic_metric_reader_builder_t* otel_periodic_metric_reader_builder_new(void);
void otel_periodic_metric_reader_builder_destroy(otel_periodic_metric_reader_builder_t* builder);
/*
 * Select the reader implementation. BLOCKING is the default and is available in every build.
 * ASYNC requires the `metrics-async-runtime` Cargo feature and owns one bounded Tokio worker.
 * The current blocking OTLP/HTTP and synchronous OTLP/gRPC wrappers are intentionally
 * incompatible with ASYNC. ASYNC currently supports custom exporters.
 */
otel_status_t otel_periodic_metric_reader_builder_set_runtime(
    otel_periodic_metric_reader_builder_t* builder, otel_metric_reader_runtime_t runtime);
otel_status_t otel_periodic_metric_reader_builder_set_interval_millis(
    otel_periodic_metric_reader_builder_t* builder, uint64_t interval_millis);
/*
 * Configure the upstream cooperative per-export timeout for the ASYNC reader. Zero selects the
 * upstream default. It is enforced only while an exporter future yields; it does not preempt
 * synchronous custom callback execution. A non-zero timeout with the BLOCKING reader is rejected
 * at build time.
 */
otel_status_t otel_periodic_metric_reader_builder_set_timeout_millis(
    otel_periodic_metric_reader_builder_t* builder, uint64_t timeout_millis);
otel_status_t otel_periodic_metric_reader_builder_set_exporter(
    otel_periodic_metric_reader_builder_t* builder, otel_metric_exporter_t* exporter);
otel_status_t otel_periodic_metric_reader_builder_build(
    otel_periodic_metric_reader_builder_t* builder, otel_periodic_metric_reader_t** out);
void otel_periodic_metric_reader_destroy(otel_periodic_metric_reader_t* reader);

#ifdef __cplusplus
}
#endif
#endif
