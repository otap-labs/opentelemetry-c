#ifndef OPENTELEMETRY_C_OTLP_METRIC_EXPORTER_H
#define OPENTELEMETRY_C_OTLP_METRIC_EXPORTER_H

#include <opentelemetry_c/common.h>
#include <opentelemetry_c/metric_exporter.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct otel_otlp_metric_exporter_builder_t otel_otlp_metric_exporter_builder_t;

typedef uint32_t otel_metric_temporality_t;
enum {
    OTEL_METRIC_TEMPORALITY_DEFAULT = 0,
    OTEL_METRIC_TEMPORALITY_CUMULATIVE = 1,
    OTEL_METRIC_TEMPORALITY_DELTA = 2,
    OTEL_METRIC_TEMPORALITY_LOW_MEMORY = 3
};

otel_otlp_metric_exporter_builder_t* otel_otlp_metric_exporter_builder_new(void);
void otel_otlp_metric_exporter_builder_destroy(otel_otlp_metric_exporter_builder_t* builder);
otel_status_t otel_otlp_metric_exporter_builder_set_endpoint(
    otel_otlp_metric_exporter_builder_t* builder, otel_string_view_t endpoint);
otel_status_t otel_otlp_metric_exporter_builder_add_header(
    otel_otlp_metric_exporter_builder_t* builder,
    otel_string_view_t key, otel_string_view_t value);
otel_status_t otel_otlp_metric_exporter_builder_set_timeout_millis(
    otel_otlp_metric_exporter_builder_t* builder, uint64_t timeout_millis);
otel_status_t otel_otlp_metric_exporter_builder_set_temporality(
    otel_otlp_metric_exporter_builder_t* builder, otel_metric_temporality_t temporality);
otel_status_t otel_otlp_metric_exporter_builder_build(
    const otel_otlp_metric_exporter_builder_t* builder, otel_metric_exporter_t** out);

#ifdef __cplusplus
}
#endif
#endif
