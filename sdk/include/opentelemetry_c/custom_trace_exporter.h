#ifndef OPENTELEMETRY_C_CUSTOM_TRACE_EXPORTER_H
#define OPENTELEMETRY_C_CUSTOM_TRACE_EXPORTER_H

#include <stddef.h>

#include <opentelemetry_c/common.h>
#include <opentelemetry_c/trace_exporter.h>

#ifdef __cplusplus
extern "C" {
#endif

/*
 * Callback-backed Traces exporter.
 *
 * The export callback receives a materialized, borrowed, read-only span batch view. Attribute
 * values use the same scalar tags as otel_attribute_value_t plus one-level homogeneous array
 * tags; maps are not representable in trace span attributes.
 *
 * Every pointer reachable from otel_span_export_batch_view_t is valid only for the duration
 * of the export callback. Copy anything you need before returning. Callback state must be
 * safe for the thread that drives the configured span processor. Callbacks must not shut down
 * or destroy the SDK/provider/processor/exporter currently invoking them, and must never
 * unwind across the C ABI boundary.
 *
 * user_data stays caller-owned until otel_custom_trace_exporter_new returns OTEL_STATUS_OK.
 * On success the SDK owns it and invokes state_destroy exactly once after in-flight exports
 * complete. On failure state_destroy is not invoked.
 */

enum {
    OTEL_SPAN_ATTRIBUTE_TYPE_STRING_ARRAY = 4,
    OTEL_SPAN_ATTRIBUTE_TYPE_BOOL_ARRAY = 5,
    OTEL_SPAN_ATTRIBUTE_TYPE_INT64_ARRAY = 6,
    OTEL_SPAN_ATTRIBUTE_TYPE_DOUBLE_ARRAY = 7,
    OTEL_SPAN_ATTRIBUTE_STRING_ARRAY = OTEL_SPAN_ATTRIBUTE_TYPE_STRING_ARRAY,
    OTEL_SPAN_ATTRIBUTE_BOOL_ARRAY = OTEL_SPAN_ATTRIBUTE_TYPE_BOOL_ARRAY,
    OTEL_SPAN_ATTRIBUTE_INT64_ARRAY = OTEL_SPAN_ATTRIBUTE_TYPE_INT64_ARRAY,
    OTEL_SPAN_ATTRIBUTE_DOUBLE_ARRAY = OTEL_SPAN_ATTRIBUTE_TYPE_DOUBLE_ARRAY
};

/* A batch larger than this fails the export instead of being silently truncated. */
#define OTEL_SPAN_EXPORT_MAX_SPANS ((size_t)65536)

typedef struct otel_span_array_view_t {
    const void* values;
    size_t count;
} otel_span_array_view_t;

typedef union otel_span_attribute_value_t {
    otel_attribute_value_t scalar;
    otel_span_array_view_t array;
} otel_span_attribute_value_t;

typedef struct otel_span_attribute_t {
    otel_string_view_t key;
    uint32_t value_type; /* OTEL_ATTRIBUTE_TYPE_* or OTEL_SPAN_ATTRIBUTE_TYPE_*_ARRAY. */
    otel_span_attribute_value_t value;
} otel_span_attribute_t;

typedef struct otel_span_event_view_t {
    uint64_t struct_size;
    otel_string_view_t name;
    uint64_t timestamp_unix_nanos;
    const otel_span_attribute_t* attributes;
    size_t attribute_count;
    uint32_t dropped_attributes_count;
    uint32_t reserved_flags;
    uint64_t reserved[2];
} otel_span_event_view_t;

typedef struct otel_span_export_link_view_t {
    uint64_t struct_size;
    uint8_t trace_id[16];
    uint8_t span_id[8];
    uint8_t trace_flags;
    uint8_t reserved_padding[3];
    otel_bool_t is_remote;
    otel_string_view_t trace_state;
    const otel_span_attribute_t* attributes;
    size_t attribute_count;
    uint32_t dropped_attributes_count;
    uint32_t reserved_flags;
    uint64_t reserved[2];
} otel_span_export_link_view_t;

typedef struct otel_span_export_scope_view_t {
    uint64_t struct_size;
    otel_string_view_t name;
    otel_string_view_t version;
    otel_string_view_t schema_url;
    const otel_span_attribute_t* attributes;
    size_t attribute_count;
    uint64_t reserved[2];
} otel_span_export_scope_view_t;

typedef struct otel_span_export_record_view_t {
    uint64_t struct_size;
    otel_string_view_t name;
    uint8_t trace_id[16];
    uint8_t span_id[8];
    uint8_t parent_span_id[8];
    uint8_t trace_flags;
    uint8_t reserved_padding[3];
    otel_bool_t is_remote;
    uint32_t span_kind;
    uint32_t status_code;
    uint64_t start_time_unix_nanos;
    uint64_t end_time_unix_nanos;
    otel_string_view_t status_message;
    otel_string_view_t trace_state;
    const otel_span_attribute_t* attributes;
    size_t attribute_count;
    const otel_span_event_view_t* events;
    size_t event_count;
    const otel_span_export_link_view_t* links;
    size_t link_count;
    uint32_t dropped_attributes_count;
    uint32_t dropped_events_count;
    uint32_t dropped_links_count;
    uint32_t reserved_flags;
    const otel_span_export_scope_view_t* scope;
    uint64_t reserved[4];
} otel_span_export_record_view_t;

typedef struct otel_span_export_batch_view_t {
    uint64_t struct_size;
    const otel_span_export_record_view_t* records;
    size_t record_count;
    otel_string_view_t resource_schema_url;
    const otel_span_attribute_t* resource_attributes;
    size_t resource_attribute_count;
    uint64_t reserved[4];
} otel_span_export_batch_view_t;

/*
 * export_spans is required. force_flush, shutdown, and state_destroy are optional. Set
 * struct_size to sizeof(otel_custom_trace_exporter_callbacks_t) after zero-initializing.
 * Only members covered by struct_size are read; future members will only be appended.
 */
typedef struct otel_custom_trace_exporter_callbacks_t {
    size_t struct_size;
    otel_status_t (*export_spans)(void* user_data, const otel_span_export_batch_view_t* batch);
    otel_status_t (*force_flush)(void* user_data);
    otel_status_t (*shutdown)(void* user_data, uint64_t timeout_millis);
    void (*state_destroy)(void* user_data);
} otel_custom_trace_exporter_callbacks_t;

#define OTEL_CUSTOM_TRACE_EXPORTER_CALLBACKS_REQUIRED_SIZE \
    (offsetof(otel_custom_trace_exporter_callbacks_t, export_spans) + \
     sizeof(otel_status_t (*)(void*, const otel_span_export_batch_view_t*)))

#if defined(__STDC_VERSION__) && (__STDC_VERSION__ >= 201112L) && \
    defined(UINTPTR_MAX) && (UINTPTR_MAX == 0xFFFFFFFFFFFFFFFFu)
_Static_assert(sizeof(otel_span_array_view_t) == 16,
               "otel_span_array_view_t ABI mismatch");
_Static_assert(sizeof(otel_span_attribute_value_t) == 16,
               "otel_span_attribute_value_t ABI mismatch");
_Static_assert(sizeof(otel_span_attribute_t) == 40,
               "otel_span_attribute_t ABI mismatch");
_Static_assert(sizeof(otel_span_event_view_t) == 72,
               "otel_span_event_view_t ABI mismatch");
_Static_assert(sizeof(otel_span_export_link_view_t) == 96,
               "otel_span_export_link_view_t ABI mismatch");
_Static_assert(sizeof(otel_span_export_scope_view_t) == 88,
               "otel_span_export_scope_view_t ABI mismatch");
_Static_assert(sizeof(otel_span_export_record_view_t) == 224,
               "otel_span_export_record_view_t ABI mismatch");
_Static_assert(sizeof(otel_span_export_batch_view_t) == 88,
               "otel_span_export_batch_view_t ABI mismatch");
_Static_assert(sizeof(otel_custom_trace_exporter_callbacks_t) == 40,
               "otel_custom_trace_exporter_callbacks_t ABI mismatch");
_Static_assert(OTEL_CUSTOM_TRACE_EXPORTER_CALLBACKS_REQUIRED_SIZE == 16,
               "otel_custom_trace_exporter_callbacks_t required prefix ABI mismatch");
#endif

otel_status_t otel_custom_trace_exporter_new(
    const otel_custom_trace_exporter_callbacks_t* callbacks,
    void* user_data,
    otel_trace_exporter_t** out);

#ifdef __cplusplus
}
#endif
#endif
