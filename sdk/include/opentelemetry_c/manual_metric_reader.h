// SPDX-License-Identifier: Apache-2.0

#ifndef OPENTELEMETRY_C_MANUAL_METRIC_READER_H
#define OPENTELEMETRY_C_MANUAL_METRIC_READER_H

#include <opentelemetry_c/common.h>
#include <opentelemetry_c/metric_exporter.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct otel_manual_metric_reader_t otel_manual_metric_reader_t;

/*
 * Build a reader with no background collection thread. On OTEL_STATUS_OK the exporter is
 * consumed, its original pointer becomes invalid, and ownership transfers to the reader. On
 * failure the caller still owns the exporter. After the reader is transferred to an SDK builder,
 * otel_sdk_metrics_force_flush() performs one synchronous collection/export cycle.
 */
otel_status_t otel_manual_metric_reader_new(otel_metric_exporter_t* exporter,
                                            otel_manual_metric_reader_t** out);
/*
 * Destroy an untransferred reader (no-op on NULL). Do NOT call this after an SDK builder
 * accepted the reader: successful transfer consumes it and invalidates the original pointer.
 */
void otel_manual_metric_reader_destroy(otel_manual_metric_reader_t* reader);

#ifdef __cplusplus
}
#endif
#endif
