#ifndef OPENTELEMETRY_C_METRIC_EXPORTER_H
#define OPENTELEMETRY_C_METRIC_EXPORTER_H

#include <opentelemetry_c/common.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct otel_metric_exporter_t otel_metric_exporter_t;
void otel_metric_exporter_destroy(otel_metric_exporter_t* exporter);

#ifdef __cplusplus
}
#endif
#endif
