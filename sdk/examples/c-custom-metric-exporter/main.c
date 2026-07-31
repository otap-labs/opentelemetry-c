#include <opentelemetry_c/manual_metric_reader.h>
#include <opentelemetry_c/metric_exporter.h>
#include <opentelemetry_c/metrics.h>
#include <opentelemetry_c/sdk.h>

#include <inttypes.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

typedef struct exporter_state_t {
    pthread_mutex_t lock;
    uint32_t current_data_kind;
    uint32_t current_number_kind;
    unsigned long exports;
    unsigned long flushes;
    unsigned long shutdowns;
} exporter_state_t;

static void print_status_error(const char* what, otel_status_t status) {
    otel_string_view_t message = otel_last_error_message();
    fprintf(stderr, "%s failed: status=%u", what, (unsigned)status);
    if (message.ptr != NULL && message.len > 0) {
        fprintf(stderr, " (%.*s)", (int)message.len, message.ptr);
    }
    fputc('\n', stderr);
}

static void print_view(otel_string_view_t view) {
    if (view.ptr == NULL || view.len == 0) {
        printf("<empty>");
        return;
    }
    printf("%.*s", (int)view.len, view.ptr);
}

static void print_number(uint32_t number_kind, otel_metric_number_t number) {
    switch (number_kind) {
    case OTEL_METRIC_NUMBER_U64:
        printf("%" PRIu64, number.u64_value);
        break;
    case OTEL_METRIC_NUMBER_I64:
        printf("%" PRId64, number.i64_value);
        break;
    case OTEL_METRIC_NUMBER_F64:
        printf("%f", number.f64_value);
        break;
    default:
        printf("<unknown>");
        break;
    }
}

static void print_attributes(const otel_metric_attribute_t* attributes, size_t attribute_count) {
    if (attribute_count == 0 || attributes == NULL) {
        printf("          attributes: <none>\n");
        return;
    }
    printf("          attributes:\n");
    for (size_t i = 0; i < attribute_count; i++) {
        const otel_metric_attribute_t* attribute = &attributes[i];
        printf("            ");
        print_view(attribute->key);
        printf(" = ");
        switch (attribute->value_type) {
        case OTEL_ATTRIBUTE_TYPE_STRING:
            print_view(attribute->value.scalar.string_value);
            break;
        case OTEL_ATTRIBUTE_TYPE_BOOL:
            printf("%s", attribute->value.scalar.bool_value ? "true" : "false");
            break;
        case OTEL_ATTRIBUTE_TYPE_INT64:
            printf("%" PRId64, attribute->value.scalar.int64_value);
            break;
        case OTEL_ATTRIBUTE_TYPE_DOUBLE:
            printf("%f", attribute->value.scalar.double_value);
            break;
        case OTEL_METRIC_ATTRIBUTE_TYPE_STRING_ARRAY: {
            const otel_string_view_t* values =
                (const otel_string_view_t*)attribute->value.array.values;
            printf("[");
            for (size_t j = 0; j < attribute->value.array.count; j++) {
                if (j > 0) {
                    printf(", ");
                }
                print_view(values[j]);
            }
            printf("]");
            break;
        }
        case OTEL_METRIC_ATTRIBUTE_TYPE_BOOL_ARRAY: {
            const otel_bool_t* values = (const otel_bool_t*)attribute->value.array.values;
            printf("[");
            for (size_t j = 0; j < attribute->value.array.count; j++) {
                if (j > 0) {
                    printf(", ");
                }
                printf("%s", values[j] ? "true" : "false");
            }
            printf("]");
            break;
        }
        case OTEL_METRIC_ATTRIBUTE_TYPE_INT64_ARRAY: {
            const int64_t* values = (const int64_t*)attribute->value.array.values;
            printf("[");
            for (size_t j = 0; j < attribute->value.array.count; j++) {
                if (j > 0) {
                    printf(", ");
                }
                printf("%" PRId64, values[j]);
            }
            printf("]");
            break;
        }
        case OTEL_METRIC_ATTRIBUTE_TYPE_DOUBLE_ARRAY: {
            const double* values = (const double*)attribute->value.array.values;
            printf("[");
            for (size_t j = 0; j < attribute->value.array.count; j++) {
                if (j > 0) {
                    printf(", ");
                }
                printf("%f", values[j]);
            }
            printf("]");
            break;
        }
        default:
            printf("<unsupported-type:%u>", (unsigned)attribute->value_type);
            break;
        }
        printf("\n");
    }
}

static otel_status_t visit_resource(void* user_data,
                                    const otel_metric_attribute_t* attributes,
                                    size_t attribute_count) {
    (void)user_data;
    printf("resource\n");
    print_attributes(attributes, attribute_count);
    return OTEL_STATUS_OK;
}

static otel_status_t visit_scope(void* user_data,
                                 otel_string_view_t name,
                                 otel_string_view_t version,
                                 otel_string_view_t schema_url,
                                 const otel_metric_attribute_t* attributes,
                                 size_t attribute_count) {
    (void)user_data;
    printf("  scope\n");
    printf("    name: ");
    print_view(name);
    printf("\n    version: ");
    print_view(version);
    printf("\n    schema_url: ");
    print_view(schema_url);
    printf("\n");
    print_attributes(attributes, attribute_count);
    return OTEL_STATUS_OK;
}

static otel_status_t visit_metric(void* user_data, const otel_metric_metadata_t* metadata) {
    exporter_state_t* state = (exporter_state_t*)user_data;
    if (metadata == NULL) {
        return OTEL_STATUS_INVALID_ARGUMENT;
    }
    state->current_data_kind = metadata->data_kind;
    state->current_number_kind = metadata->number_kind;
    printf("    metric\n");
    printf("      name: ");
    print_view(metadata->name);
    printf("\n      description: ");
    print_view(metadata->description);
    printf("\n      unit: ");
    print_view(metadata->unit);
    printf("\n      data_kind=%u number_kind=%u\n",
           (unsigned)metadata->data_kind,
           (unsigned)metadata->number_kind);
    return OTEL_STATUS_OK;
}

static otel_status_t visit_point(void* user_data,
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
                                 size_t negative_bucket_count) {
    exporter_state_t* state = (exporter_state_t*)user_data;
    (void)positive_bucket_counts;
    (void)negative_bucket_counts;
    if (point == NULL) {
        return OTEL_STATUS_INVALID_ARGUMENT;
    }
    printf("      point[%zu]\n", point->point_index);
    printf("        start=%" PRIu64 " end=%" PRIu64 "\n",
           point->start_time_unix_nanos,
           point->time_unix_nanos);
    print_attributes(attributes, attribute_count);

    if (state->current_data_kind == OTEL_METRIC_DATA_GAUGE ||
        state->current_data_kind == OTEL_METRIC_DATA_SUM) {
        printf("        value=");
        print_number(state->current_number_kind, point->value);
        printf(" monotonic=%u temporality=%u\n",
               (unsigned)point->is_monotonic,
               (unsigned)point->temporality);
    } else if (state->current_data_kind == OTEL_METRIC_DATA_HISTOGRAM) {
        printf("        histogram count=%" PRIu64 " sum=", point->count);
        print_number(state->current_number_kind, point->sum);
        if (point->has_min) {
            printf(" min=");
            print_number(state->current_number_kind, point->min);
        }
        if (point->has_max) {
            printf(" max=");
            print_number(state->current_number_kind, point->max);
        }
        printf("\n");
        printf("        explicit bounds/buckets:");
        for (size_t i = 0; i < explicit_bound_count && i < explicit_bucket_count; i++) {
            printf(" [<=%f]=%" PRIu64, explicit_bounds[i], explicit_bucket_counts[i]);
        }
        if (explicit_bucket_count > explicit_bound_count) {
            printf(" [+Inf]=%" PRIu64, explicit_bucket_counts[explicit_bucket_count - 1]);
        }
        printf("\n");
    } else if (state->current_data_kind == OTEL_METRIC_DATA_EXPONENTIAL_HISTOGRAM) {
        printf("        exp_hist count=%" PRIu64 " scale=%d zero_count=%" PRIu64 "\n",
               point->count,
               point->scale,
               point->zero_count);
        printf("        positive offset=%d buckets=%zu\n",
               point->positive_bucket_offset,
               positive_bucket_count);
        printf("        negative offset=%d buckets=%zu\n",
               point->negative_bucket_offset,
               negative_bucket_count);
    }
    return OTEL_STATUS_OK;
}

static otel_status_t visit_exemplar(void* user_data,
                                    const otel_metric_exemplar_t* exemplar,
                                    const otel_metric_attribute_t* filtered_attributes,
                                    size_t filtered_attribute_count) {
    exporter_state_t* state = (exporter_state_t*)user_data;
    if (exemplar == NULL) {
        return OTEL_STATUS_INVALID_ARGUMENT;
    }
    printf("        exemplar[%zu] for point[%zu] time=%" PRIu64 " value=",
           exemplar->exemplar_index,
           exemplar->point_index,
           exemplar->time_unix_nanos);
    print_number(state->current_number_kind, exemplar->value);
    printf("\n");
    print_attributes(filtered_attributes, filtered_attribute_count);
    return OTEL_STATUS_OK;
}

static otel_status_t export_metrics(void* user_data, const otel_metric_batch_t* batch) {
    exporter_state_t* state = (exporter_state_t*)user_data;
    otel_metric_visitor_t visitor;
    otel_status_t status;

    if (batch == NULL || state == NULL) {
        return OTEL_STATUS_INVALID_ARGUMENT;
    }

    /*
     * All views reachable from otel_metric_batch_visit are borrowed and valid only during
     * this callback: batch token, visitor data pointers, metric/point/exemplar structs,
     * attribute arrays, and string views. Do not retain them after this callback returns.
     */
    memset(&visitor, 0, sizeof(visitor));
    visitor.struct_size = sizeof(visitor);
    visitor.resource = visit_resource;
    visitor.scope = visit_scope;
    visitor.metric = visit_metric;
    visitor.point = visit_point;
    visitor.exemplar = visit_exemplar;

    pthread_mutex_lock(&state->lock);
    state->exports += 1;
    printf("=== export batch #%lu ===\n", state->exports);
    status = otel_metric_batch_visit(batch, &visitor, user_data);
    pthread_mutex_unlock(&state->lock);
    return status;
}

static otel_status_t exporter_force_flush(void* user_data) {
    exporter_state_t* state = (exporter_state_t*)user_data;
    pthread_mutex_lock(&state->lock);
    state->flushes += 1;
    printf("force_flush callback #%lu\n", state->flushes);
    pthread_mutex_unlock(&state->lock);
    return OTEL_STATUS_OK;
}

static otel_status_t exporter_shutdown(void* user_data, uint64_t timeout_millis) {
    exporter_state_t* state = (exporter_state_t*)user_data;
    pthread_mutex_lock(&state->lock);
    state->shutdowns += 1;
    printf("shutdown callback #%lu timeout=%" PRIu64 "ms\n", state->shutdowns, timeout_millis);
    pthread_mutex_unlock(&state->lock);
    return OTEL_STATUS_OK;
}

static void exporter_state_destroy(void* user_data) {
    exporter_state_t* state = (exporter_state_t*)user_data;
    if (state == NULL) {
        return;
    }
    pthread_mutex_destroy(&state->lock);
    free(state);
    printf("state_destroy callback completed\n");
}

int main(void) {
    int result = 1;
    exporter_state_t* state = NULL;
    otel_custom_metric_exporter_callbacks_t callbacks;
    otel_metric_exporter_t* exporter = NULL;
    otel_manual_metric_reader_t* reader = NULL;
    otel_sdk_builder_t* sdk_builder = NULL;
    otel_sdk_t* sdk = NULL;
    otel_meter_provider_t* provider = NULL;
    otel_meter_t* meter = NULL;
    otel_counter_u64_t* counter = NULL;
    otel_gauge_f64_t* gauge = NULL;
    otel_histogram_f64_t* histogram = NULL;
    otel_status_t status;

    state = (exporter_state_t*)calloc(1, sizeof(exporter_state_t));
    if (state == NULL) {
        fprintf(stderr, "failed to allocate exporter state\n");
        goto cleanup;
    }
    if (pthread_mutex_init(&state->lock, NULL) != 0) {
        fprintf(stderr, "failed to initialize exporter state mutex\n");
        goto cleanup;
    }

    memset(&callbacks, 0, sizeof(callbacks));
    callbacks.struct_size = sizeof(callbacks);
    callbacks.export_metrics = export_metrics;
    callbacks.force_flush = exporter_force_flush;
    callbacks.shutdown = exporter_shutdown;
    callbacks.state_destroy = exporter_state_destroy;

    status = otel_custom_metric_exporter_new(
        &callbacks, state, OTEL_METRIC_TEMPORALITY_CUMULATIVE, &exporter);
    if (status != OTEL_STATUS_OK) {
        print_status_error("otel_custom_metric_exporter_new", status);
        goto cleanup;
    }
    state = NULL; /* ownership transferred on success */

    status = otel_manual_metric_reader_new(exporter, &reader);
    if (status != OTEL_STATUS_OK) {
        print_status_error("otel_manual_metric_reader_new", status);
        goto cleanup;
    }
    exporter = NULL;

    sdk_builder = otel_sdk_builder_new();
    if (sdk_builder == NULL) {
        fprintf(stderr, "otel_sdk_builder_new failed\n");
        goto cleanup;
    }
    status =
        otel_sdk_builder_set_service_name(sdk_builder, otel_cstr("c-custom-metric-exporter"));
    if (status != OTEL_STATUS_OK) {
        print_status_error("otel_sdk_builder_set_service_name", status);
        goto cleanup;
    }
    status = otel_sdk_builder_add_manual_metric_reader(sdk_builder, reader);
    if (status != OTEL_STATUS_OK) {
        print_status_error("otel_sdk_builder_add_manual_metric_reader", status);
        goto cleanup;
    }
    reader = NULL;

    status = otel_sdk_build(sdk_builder, &sdk);
    if (status != OTEL_STATUS_OK || sdk == NULL) {
        print_status_error("otel_sdk_build", status);
        goto cleanup;
    }
    otel_sdk_builder_destroy(sdk_builder);
    sdk_builder = NULL;
    status = otel_sdk_set_metrics_as_global(sdk);
    if (status != OTEL_STATUS_OK) {
        print_status_error("otel_sdk_set_metrics_as_global", status);
        goto cleanup;
    }

    provider = otel_global_meter_provider();
    meter = otel_meter_provider_get_meter(provider, otel_cstr("example.custom.exporter"),
                                          otel_cstr("1.0.0"), otel_string_view_empty());
    if (provider == NULL || meter == NULL) {
        fprintf(stderr, "failed to acquire provider/meter\n");
        goto cleanup;
    }

    otel_instrument_options_t options = OTEL_INSTRUMENT_OPTIONS_INIT;
    options.description = otel_cstr("requests served");
    options.unit = otel_cstr("1");
    status =
        otel_meter_create_u64_counter(meter, otel_cstr("requests.total"), &options, &counter);
    if (status != OTEL_STATUS_OK || counter == NULL) {
        print_status_error("otel_meter_create_u64_counter", status);
        goto cleanup;
    }
    status = otel_meter_create_f64_gauge(meter, otel_cstr("queue.depth"), &options, &gauge);
    if (status != OTEL_STATUS_OK || gauge == NULL) {
        print_status_error("otel_meter_create_f64_gauge", status);
        goto cleanup;
    }

    double boundaries[] = { 5.0, 10.0, 25.0 };
    otel_instrument_options_t histogram_options = options;
    histogram_options.boundaries = boundaries;
    histogram_options.boundary_count = sizeof(boundaries) / sizeof(boundaries[0]);
    status = otel_meter_create_f64_histogram(
        meter, otel_cstr("request.duration.ms"), &histogram_options, &histogram);
    if (status != OTEL_STATUS_OK || histogram == NULL) {
        print_status_error("otel_meter_create_f64_histogram", status);
        goto cleanup;
    }

    otel_key_value_t attrs[] = {
        otel_kv_string(otel_cstr("route"), otel_cstr("/orders")),
        otel_kv_int64(otel_cstr("status"), 200)
    };
    status = otel_counter_u64_add(counter, 3, attrs, 2);
    if (status != OTEL_STATUS_OK) {
        print_status_error("otel_counter_u64_add", status);
        goto cleanup;
    }
    status = otel_gauge_f64_record(gauge, 4.0, attrs, 2);
    if (status != OTEL_STATUS_OK) {
        print_status_error("otel_gauge_f64_record", status);
        goto cleanup;
    }
    status = otel_histogram_f64_record(histogram, 12.5, attrs, 2);
    if (status != OTEL_STATUS_OK) {
        print_status_error("otel_histogram_f64_record", status);
        goto cleanup;
    }

    status = otel_sdk_metrics_force_flush(sdk, 0);
    if (status != OTEL_STATUS_OK) {
        print_status_error("otel_sdk_metrics_force_flush", status);
        goto cleanup;
    }
    status = otel_sdk_metrics_shutdown(sdk, 5000);
    if (status != OTEL_STATUS_OK) {
        print_status_error("otel_sdk_metrics_shutdown", status);
        goto cleanup;
    }
    printf("custom metric exporter example completed\n");
    result = 0;

cleanup:
    otel_histogram_f64_destroy(histogram);
    otel_gauge_f64_destroy(gauge);
    otel_counter_u64_destroy(counter);
    otel_meter_destroy(meter);
    otel_meter_provider_destroy(provider);
    otel_sdk_destroy(sdk);
    otel_sdk_builder_destroy(sdk_builder);
    otel_manual_metric_reader_destroy(reader);
    otel_metric_exporter_destroy(exporter);
    if (state != NULL) {
        pthread_mutex_destroy(&state->lock);
        free(state);
    }
    return result;
}
