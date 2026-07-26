#ifndef OPENTELEMETRY_C_METRIC_EXPORTER_H
#define OPENTELEMETRY_C_METRIC_EXPORTER_H

#include <opentelemetry_c/common.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct otel_metric_exporter_t otel_metric_exporter_t;
typedef struct otel_metric_batch_t otel_metric_batch_t;

typedef uint32_t otel_metric_temporality_t;
enum {
    OTEL_METRIC_TEMPORALITY_DEFAULT = 0,
    OTEL_METRIC_TEMPORALITY_CUMULATIVE = 1,
    OTEL_METRIC_TEMPORALITY_DELTA = 2,
    OTEL_METRIC_TEMPORALITY_LOW_MEMORY = 3
};

typedef uint32_t otel_metric_number_kind_t;
enum {
    OTEL_METRIC_NUMBER_U64 = 0,
    OTEL_METRIC_NUMBER_I64 = 1,
    OTEL_METRIC_NUMBER_F64 = 2
};

typedef uint32_t otel_metric_data_kind_t;
enum {
    OTEL_METRIC_DATA_GAUGE = 0,
    OTEL_METRIC_DATA_SUM = 1,
    OTEL_METRIC_DATA_HISTOGRAM = 2,
    OTEL_METRIC_DATA_EXPONENTIAL_HISTOGRAM = 3
};

enum {
    OTEL_METRIC_ATTRIBUTE_TYPE_STRING_ARRAY = 4,
    OTEL_METRIC_ATTRIBUTE_TYPE_BOOL_ARRAY = 5,
    OTEL_METRIC_ATTRIBUTE_TYPE_INT64_ARRAY = 6,
    OTEL_METRIC_ATTRIBUTE_TYPE_DOUBLE_ARRAY = 7
};

typedef struct otel_metric_array_view_t {
    const void* values;
    size_t count;
} otel_metric_array_view_t;

typedef union otel_metric_attribute_value_t {
    otel_attribute_value_t scalar;
    otel_metric_array_view_t array;
} otel_metric_attribute_value_t;

typedef struct otel_metric_attribute_t {
    otel_string_view_t key;
    uint32_t value_type; /* OTEL_ATTRIBUTE_TYPE_* or OTEL_METRIC_ATTRIBUTE_TYPE_*_ARRAY. */
    otel_metric_attribute_value_t value;
} otel_metric_attribute_t;

typedef union otel_metric_number_t {
    uint64_t u64_value;
    int64_t i64_value;
    double f64_value;
} otel_metric_number_t;

typedef struct otel_metric_metadata_t {
    otel_string_view_t name;
    otel_string_view_t description;
    otel_string_view_t unit;
    otel_metric_data_kind_t data_kind;
    otel_metric_number_kind_t number_kind;
} otel_metric_metadata_t;

typedef struct otel_metric_point_t {
    size_t point_index;
    uint64_t start_time_unix_nanos;
    uint64_t time_unix_nanos;
    otel_metric_temporality_t temporality;
    otel_bool_t is_monotonic;
    otel_metric_number_t value;
    uint64_t count;
    otel_metric_number_t sum;
    otel_metric_number_t min;
    otel_metric_number_t max;
    otel_bool_t has_min;
    otel_bool_t has_max;
    int8_t scale;
    uint8_t _padding[7];
    uint64_t zero_count;
    double zero_threshold;
    int32_t positive_bucket_offset;
    int32_t negative_bucket_offset;
} otel_metric_point_t;

typedef struct otel_metric_exemplar_t {
    size_t point_index;
    size_t exemplar_index;
    uint64_t time_unix_nanos;
    otel_metric_number_t value;
    uint8_t span_id[8];
    uint8_t trace_id[16];
} otel_metric_exemplar_t;

/*
 * Every pointer supplied to a visitor callback is borrowed only until that callback returns.
 * Metric batches are callback-thread-local tokens: otel_metric_batch_visit() rejects stale
 * and cross-thread use. Visitor callbacks run synchronously on the exporter callback's thread
 * and must not retain any pointer or invoke otel_metric_batch_visit() from another thread.
 *
 * The point callback receives explicit histogram bounds/counts and exponential histogram
 * positive/negative counts. Arrays that do not apply to the current data_kind are NULL/zero.
 * Attribute arrays use element types const otel_string_view_t, const otel_bool_t,
 * const int64_t, or const double according to value_type. Gauge and sum points use `value`;
 * histogram points use count/sum/min/max and the explicit arrays; exponential histograms use
 * count/sum/min/max, scale/zero fields, bucket offsets, and the positive/negative arrays.
 * A zero start_time_unix_nanos means the upstream aggregation supplied no start time.
 */
typedef struct otel_metric_visitor_t {
    size_t struct_size;
    otel_status_t (*resource)(void* visitor_data,
                              const otel_metric_attribute_t* attributes,
                              size_t attribute_count);
    otel_status_t (*scope)(void* visitor_data,
                           otel_string_view_t name,
                           otel_string_view_t version,
                           otel_string_view_t schema_url,
                           const otel_metric_attribute_t* attributes,
                           size_t attribute_count);
    otel_status_t (*metric)(void* visitor_data,
                            const otel_metric_metadata_t* metadata);
    otel_status_t (*point)(void* visitor_data,
                           const otel_metric_point_t* point,
                           const otel_metric_attribute_t* attributes,
                           size_t attribute_count,
                           const double* explicit_bounds,
                           size_t explicit_bound_count,
                           const uint64_t* explicit_bucket_counts,
                           size_t explicit_bucket_count,
                           const uint64_t* positive_bucket_counts,
                           size_t positive_bucket_count,
                           const uint64_t* negative_bucket_counts,
                           size_t negative_bucket_count);
    otel_status_t (*exemplar)(void* visitor_data,
                              const otel_metric_exemplar_t* exemplar,
                              const otel_metric_attribute_t* filtered_attributes,
                              size_t filtered_attribute_count);
} otel_metric_visitor_t;

otel_status_t otel_metric_batch_visit(const otel_metric_batch_t* batch,
                                      const otel_metric_visitor_t* visitor,
                                      void* visitor_data);

/*
 * Custom exporter callbacks may be invoked from SDK collection threads or from the caller of
 * otel_sdk_metrics_force_flush(). Different SDKs/readers may invoke the same callback state
 * concurrently; callback state must therefore be thread-safe. Callbacks must not call Metrics
 * provider force-flush/shutdown/destroy reentrantly. A non-zero callback status fails that
 * export operation and is surfaced as OTEL_STATUS_EXPORT_FAILED at the provider lifecycle
 * boundary.
 *
 * On successful construction the SDK owns user_data and invokes state_destroy exactly once,
 * after all exporter callbacks have stopped. On construction failure ownership remains with
 * the caller. force_flush and shutdown are optional; export_metrics is required.
 */
typedef struct otel_custom_metric_exporter_callbacks_t {
    size_t struct_size;
    otel_status_t (*export_metrics)(void* user_data, const otel_metric_batch_t* batch);
    otel_status_t (*force_flush)(void* user_data);
    otel_status_t (*shutdown)(void* user_data, uint64_t timeout_millis);
    void (*state_destroy)(void* user_data);
} otel_custom_metric_exporter_callbacks_t;

#if defined(__STDC_VERSION__) && (__STDC_VERSION__ >= 201112L) && \
    defined(UINTPTR_MAX) && (UINTPTR_MAX == 0xFFFFFFFFFFFFFFFFu)
_Static_assert(sizeof(otel_metric_array_view_t) == 16,
               "otel_metric_array_view_t ABI mismatch");
_Static_assert(sizeof(otel_metric_attribute_t) == 40,
               "otel_metric_attribute_t ABI mismatch");
_Static_assert(sizeof(otel_metric_metadata_t) == 56,
               "otel_metric_metadata_t ABI mismatch");
_Static_assert(sizeof(otel_metric_point_t) == 112,
               "otel_metric_point_t ABI mismatch");
_Static_assert(sizeof(otel_metric_exemplar_t) == 56,
               "otel_metric_exemplar_t ABI mismatch");
_Static_assert(sizeof(otel_metric_visitor_t) == 48,
               "otel_metric_visitor_t ABI mismatch");
_Static_assert(sizeof(otel_custom_metric_exporter_callbacks_t) == 40,
               "otel_custom_metric_exporter_callbacks_t ABI mismatch");
#endif

otel_status_t otel_custom_metric_exporter_new(
    const otel_custom_metric_exporter_callbacks_t* callbacks,
    void* user_data,
    otel_metric_temporality_t temporality,
    otel_metric_exporter_t** out);

/*
 * Destroy an untransferred exporter (no-op on NULL). Do NOT call this after a reader accepted
 * the exporter: successful transfer consumes the handle and invalidates the original pointer.
 */
void otel_metric_exporter_destroy(otel_metric_exporter_t* exporter);

#ifdef __cplusplus
}
#endif
#endif
