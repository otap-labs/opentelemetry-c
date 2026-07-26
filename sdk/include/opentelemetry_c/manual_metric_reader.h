#ifndef OPENTELEMETRY_C_MANUAL_METRIC_READER_H
#define OPENTELEMETRY_C_MANUAL_METRIC_READER_H

#include <opentelemetry_c/common.h>
#include <opentelemetry_c/metric_exporter.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct otel_manual_metric_reader_t otel_manual_metric_reader_t;

/*
 * Build a reader with no background collection thread. On OTEL_STATUS_OK ownership of the
 * exporter transfers to the reader. After the reader is transferred to an SDK builder,
 * otel_sdk_metrics_force_flush() performs one synchronous collection/export cycle.
 */
otel_status_t otel_manual_metric_reader_new(otel_metric_exporter_t* exporter,
                                            otel_manual_metric_reader_t** out);
void otel_manual_metric_reader_destroy(otel_manual_metric_reader_t* reader);

#ifdef __cplusplus
}
#endif
#endif
