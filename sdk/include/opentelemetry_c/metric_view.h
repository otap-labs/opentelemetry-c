#ifndef OPENTELEMETRY_C_METRIC_VIEW_H
#define OPENTELEMETRY_C_METRIC_VIEW_H

#include <opentelemetry_c/common.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct otel_metric_view_builder_t otel_metric_view_builder_t;
typedef struct otel_metric_view_t otel_metric_view_t;

typedef uint32_t otel_metric_instrument_kind_t;
enum {
    OTEL_METRIC_INSTRUMENT_COUNTER = 0,
    OTEL_METRIC_INSTRUMENT_UP_DOWN_COUNTER = 1,
    OTEL_METRIC_INSTRUMENT_GAUGE = 2,
    OTEL_METRIC_INSTRUMENT_HISTOGRAM = 3,
    OTEL_METRIC_INSTRUMENT_OBSERVABLE_COUNTER = 4,
    OTEL_METRIC_INSTRUMENT_OBSERVABLE_UP_DOWN_COUNTER = 5,
    OTEL_METRIC_INSTRUMENT_OBSERVABLE_GAUGE = 6,
    OTEL_METRIC_INSTRUMENT_ANY = UINT32_MAX
};

typedef uint32_t otel_metric_aggregation_t;
enum {
    OTEL_METRIC_AGGREGATION_DEFAULT = 0,
    OTEL_METRIC_AGGREGATION_DROP = 1,
    OTEL_METRIC_AGGREGATION_SUM = 2,
    OTEL_METRIC_AGGREGATION_LAST_VALUE = 3
};

otel_metric_view_builder_t* otel_metric_view_builder_new(void);
void otel_metric_view_builder_destroy(otel_metric_view_builder_t* builder);
otel_status_t otel_metric_view_builder_set_name_pattern(
    otel_metric_view_builder_t* builder, otel_string_view_t pattern);
otel_status_t otel_metric_view_builder_set_meter_name(
    otel_metric_view_builder_t* builder, otel_string_view_t meter_name);
otel_status_t otel_metric_view_builder_set_scope_version(
    otel_metric_view_builder_t* builder, otel_string_view_t version);
otel_status_t otel_metric_view_builder_set_scope_schema_url(
    otel_metric_view_builder_t* builder, otel_string_view_t schema_url);
/*
 * Add a required exact scope-attribute matcher. Every configured key/value must be present
 * with the same value type for the view to match; extra scope attributes are allowed.
 * Duplicate matcher keys are rejected.
 */
otel_status_t otel_metric_view_builder_add_scope_attribute(
    otel_metric_view_builder_t* builder, const otel_key_value_t* attribute);
otel_status_t otel_metric_view_builder_set_unit(
    otel_metric_view_builder_t* builder, otel_string_view_t unit);
otel_status_t otel_metric_view_builder_set_instrument_kind(
    otel_metric_view_builder_t* builder, otel_metric_instrument_kind_t kind);
otel_status_t otel_metric_view_builder_set_output_name(
    otel_metric_view_builder_t* builder, otel_string_view_t name);
otel_status_t otel_metric_view_builder_set_output_description(
    otel_metric_view_builder_t* builder, otel_string_view_t description);
otel_status_t otel_metric_view_builder_set_output_unit(
    otel_metric_view_builder_t* builder, otel_string_view_t unit);
otel_status_t otel_metric_view_builder_add_allowed_attribute(
    otel_metric_view_builder_t* builder, otel_string_view_t key);
/* Enable filtering explicitly. When enabled with no allowed keys, all attributes are
 * dropped. Adding an allowed key also enables filtering. */
otel_status_t otel_metric_view_builder_set_attribute_filter_enabled(
    otel_metric_view_builder_t* builder, otel_bool_t enabled);
otel_status_t otel_metric_view_builder_set_cardinality_limit(
    otel_metric_view_builder_t* builder, uint64_t limit);
otel_status_t otel_metric_view_builder_set_aggregation(
    otel_metric_view_builder_t* builder, otel_metric_aggregation_t aggregation);
otel_status_t otel_metric_view_builder_set_explicit_histogram(
    otel_metric_view_builder_t* builder, const double* boundaries, size_t count,
    otel_bool_t record_min_max);
otel_status_t otel_metric_view_builder_set_exponential_histogram(
    otel_metric_view_builder_t* builder, uint32_t max_size, int8_t max_scale,
    otel_bool_t record_min_max);
otel_status_t otel_metric_view_builder_build(
    otel_metric_view_builder_t* builder, otel_metric_view_t** out);
void otel_metric_view_destroy(otel_metric_view_t* view);

#ifdef __cplusplus
}
#endif
#endif
