#ifndef OPENTELEMETRY_C_PERIODIC_METRIC_READER_H
#define OPENTELEMETRY_C_PERIODIC_METRIC_READER_H

#include <opentelemetry_c/common.h>
#include <opentelemetry_c/metric_exporter.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct otel_periodic_metric_reader_builder_t otel_periodic_metric_reader_builder_t;
typedef struct otel_periodic_metric_reader_t otel_periodic_metric_reader_t;

/*
 * The pinned OpenTelemetry Rust 0.32 PeriodicReader uses a fixed five-second exporter
 * timeout while shutting down. otel_sdk_metrics_shutdown() cannot override that reader
 * timeout; its timeout_millis argument is currently advisory for Metrics.
 */
otel_periodic_metric_reader_builder_t* otel_periodic_metric_reader_builder_new(void);
void otel_periodic_metric_reader_builder_destroy(otel_periodic_metric_reader_builder_t* builder);
otel_status_t otel_periodic_metric_reader_builder_set_interval_millis(
    otel_periodic_metric_reader_builder_t* builder, uint64_t interval_millis);
otel_status_t otel_periodic_metric_reader_builder_set_exporter(
    otel_periodic_metric_reader_builder_t* builder, otel_metric_exporter_t* exporter);
otel_status_t otel_periodic_metric_reader_builder_build(
    otel_periodic_metric_reader_builder_t* builder, otel_periodic_metric_reader_t** out);
void otel_periodic_metric_reader_destroy(otel_periodic_metric_reader_t* reader);

#ifdef __cplusplus
}
#endif
#endif
