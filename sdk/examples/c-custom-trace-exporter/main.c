/*
 * Callback-backed Traces exporter: receive finished span batches in your own C code.
 *
 * Like the custom logs and metrics exporters, this needs no OTLP transport, no network, and
 * no extra feature flags. The SDK converts each finished span batch into a materialized,
 * borrowed, read-only view and hands it to a callback that prints it.
 *
 * Every pointer reachable from otel_span_export_batch_view_t is valid only for the duration
 * of the export callback. This example therefore prints during the callback and copies
 * nothing out of it.
 *
 * Spans are emitted through the SDK's own tracer provider so the whole pipeline is released
 * on otel_sdk_destroy, which makes the callback-state lifecycle (shutdown, then state_destroy)
 * observable. For the split API-only-instrumentation model with a globally installed SDK, see
 * the c-basic-traces example.
 */

#include <opentelemetry_c/custom_trace_exporter.h> /* callback-backed span exporter */
#include <opentelemetry_c/sdk.h>                    /* SDK: builder + lifecycle */
#include <opentelemetry_c/simple_span_processor.h>  /* export on the emitting thread */
#include <opentelemetry_c/trace.h>                  /* tracer + span API */

#include <stdint.h>
#include <stdio.h>
#include <string.h>

typedef struct exporter_state_t {
    unsigned long batches;
    unsigned long spans;
} exporter_state_t;

static void print_view(otel_string_view_t view) {
    printf("%.*s", (int)view.len, view.len == 0 ? "" : view.ptr);
}

static void print_scalar(uint32_t value_type, const otel_attribute_value_t* scalar) {
    switch (value_type) {
    case OTEL_ATTRIBUTE_TYPE_STRING:
        printf("\"");
        print_view(scalar->string_value);
        printf("\"");
        break;
    case OTEL_ATTRIBUTE_TYPE_BOOL:
        printf(scalar->bool_value ? "true" : "false");
        break;
    case OTEL_ATTRIBUTE_TYPE_INT64:
        printf("%lld", (long long)scalar->int64_value);
        break;
    case OTEL_ATTRIBUTE_TYPE_DOUBLE:
        printf("%f", scalar->double_value);
        break;
    default:
        printf("<unknown>");
        break;
    }
}

/* Print one span attribute, handling scalar tags and one-level homogeneous array tags. */
static void print_attribute(const otel_span_attribute_t* attr) {
    size_t i;

    print_view(attr->key);
    printf("=");
    switch (attr->value_type) {
    case OTEL_SPAN_ATTRIBUTE_TYPE_STRING_ARRAY: {
        const otel_string_view_t* items = (const otel_string_view_t*)attr->value.array.values;
        printf("[");
        for (i = 0; i < attr->value.array.count; i++) {
            if (i > 0) {
                printf(", ");
            }
            printf("\"");
            print_view(items[i]);
            printf("\"");
        }
        printf("]");
        break;
    }
    case OTEL_SPAN_ATTRIBUTE_TYPE_BOOL_ARRAY: {
        const otel_bool_t* items = (const otel_bool_t*)attr->value.array.values;
        printf("[");
        for (i = 0; i < attr->value.array.count; i++) {
            printf("%s%s", i > 0 ? ", " : "", items[i] ? "true" : "false");
        }
        printf("]");
        break;
    }
    case OTEL_SPAN_ATTRIBUTE_TYPE_INT64_ARRAY: {
        const int64_t* items = (const int64_t*)attr->value.array.values;
        printf("[");
        for (i = 0; i < attr->value.array.count; i++) {
            printf("%s%lld", i > 0 ? ", " : "", (long long)items[i]);
        }
        printf("]");
        break;
    }
    case OTEL_SPAN_ATTRIBUTE_TYPE_DOUBLE_ARRAY: {
        const double* items = (const double*)attr->value.array.values;
        printf("[");
        for (i = 0; i < attr->value.array.count; i++) {
            printf("%s%f", i > 0 ? ", " : "", items[i]);
        }
        printf("]");
        break;
    }
    default:
        print_scalar(attr->value_type, &attr->value.scalar);
        break;
    }
}

static void print_id(const uint8_t* id, size_t len) {
    size_t i;
    for (i = 0; i < len; i++) {
        printf("%02x", id[i]);
    }
}

static otel_status_t export_spans(void* user_data, const otel_span_export_batch_view_t* batch) {
    exporter_state_t* state = (exporter_state_t*)user_data;
    size_t i;
    size_t a;
    size_t e;
    size_t l;

    if (batch == NULL) {
        return OTEL_STATUS_INVALID_ARGUMENT;
    }
    state->batches++;
    printf("--- batch of %zu span(s), %zu resource attribute(s) ---\n",
           batch->record_count,
           batch->resource_attribute_count);

    for (i = 0; i < batch->record_count; i++) {
        const otel_span_export_record_view_t* record = &batch->records[i];
        state->spans++;

        printf("  span \"");
        print_view(record->name);
        printf("\" scope=");
        print_view(record->scope->name);
        printf(" kind=%u status=%u", (unsigned)record->span_kind, (unsigned)record->status_code);
        printf(" trace_id=");
        print_id(record->trace_id, sizeof(record->trace_id));
        printf(" span_id=");
        print_id(record->span_id, sizeof(record->span_id));
        printf("\n");

        for (a = 0; a < record->attribute_count; a++) {
            printf("    attr ");
            print_attribute(&record->attributes[a]);
            printf("\n");
        }

        for (e = 0; e < record->event_count; e++) {
            const otel_span_event_view_t* event = &record->events[e];
            printf("    event \"");
            print_view(event->name);
            printf("\" (%zu attr)\n", event->attribute_count);
            for (a = 0; a < event->attribute_count; a++) {
                printf("      ");
                print_attribute(&event->attributes[a]);
                printf("\n");
            }
        }

        for (l = 0; l < record->link_count; l++) {
            const otel_span_export_link_view_t* link = &record->links[l];
            printf("    link trace_id=");
            print_id(link->trace_id, sizeof(link->trace_id));
            printf(" (%zu attr)\n", link->attribute_count);
        }
    }
    return OTEL_STATUS_OK;
}

static otel_status_t force_flush_exporter(void* user_data) {
    (void)user_data;
    printf("exporter force_flush\n");
    return OTEL_STATUS_OK;
}

static otel_status_t shutdown_exporter(void* user_data, uint64_t timeout_millis) {
    exporter_state_t* state = (exporter_state_t*)user_data;
    (void)timeout_millis;
    printf("exporter shutdown after %lu batch(es), %lu span(s)\n", state->batches, state->spans);
    return OTEL_STATUS_OK;
}

/*
 * Invoked exactly once, after the last export callback has returned. A real bridge would free
 * its callback state here; this example's state lives on main()'s stack, which outlives the
 * SDK, so there is nothing to free.
 */
static void destroy_exporter_state(void* user_data) {
    (void)user_data;
    printf("exporter state released\n");
}

/* Emit spans through the SDK's tracer provider. */
static void do_instrumentation_work(otel_tracer_provider_t* provider) {
    otel_tracer_t* tracer = otel_tracer_provider_get_tracer(
        provider, otel_cstr("c-custom-trace-exporter"), otel_cstr("0.1.0"),
        otel_string_view_empty());

    otel_span_start_options_t parent_opts;
    parent_opts.kind = OTEL_SPAN_KIND_SERVER;
    parent_opts.parent = NULL;
    otel_span_t* parent =
        otel_tracer_start_span(tracer, otel_cstr("handle-request"), &parent_opts);
    otel_span_set_string_attribute(parent, otel_cstr("http.request.method"), otel_cstr("GET"));
    otel_span_set_int64_attribute(parent, otel_cstr("http.response.status_code"), 200);

    otel_key_value_t event_attr = otel_kv_string(otel_cstr("cache"), otel_cstr("miss"));
    otel_span_add_event(parent, otel_cstr("lookup"), &event_attr, 1);

    otel_span_start_options_t child_opts;
    child_opts.kind = OTEL_SPAN_KIND_CLIENT;
    child_opts.parent = parent;
    otel_span_context_t* parent_context = NULL;
    otel_span_t* child = NULL;
    if (otel_span_get_context(parent, &parent_context) == OTEL_STATUS_OK) {
        child_opts.parent = NULL;
        child = otel_tracer_start_span_with_context(
            tracer, otel_cstr("query-database"), &child_opts, parent_context);
    } else {
        child = otel_tracer_start_span(tracer, otel_cstr("query-database"), &child_opts);
    }
    otel_span_set_string_attribute(child, otel_cstr("db.system"), otel_cstr("postgresql"));
    otel_span_set_ok(child);
    otel_span_end(child);
    otel_span_destroy(child);

    otel_span_set_ok(parent);
    otel_span_end(parent);
    otel_span_destroy(parent);
    otel_span_context_destroy(parent_context);

    otel_tracer_destroy(tracer);
}

int main(void) {
    exporter_state_t state;
    otel_custom_trace_exporter_callbacks_t callbacks;
    otel_trace_exporter_t* exporter = NULL;
    otel_span_processor_t* processor = NULL;
    otel_sdk_builder_t* builder = NULL;
    otel_sdk_t* sdk = NULL;

    memset(&state, 0, sizeof(state));
    memset(&callbacks, 0, sizeof(callbacks));

    callbacks.struct_size = sizeof(callbacks);
    callbacks.export_spans = export_spans;
    callbacks.force_flush = force_flush_exporter;
    callbacks.shutdown = shutdown_exporter;
    callbacks.state_destroy = destroy_exporter_state;

    /* Ownership of `&state` transfers to the SDK only if this returns OTEL_STATUS_OK. */
    if (otel_custom_trace_exporter_new(&callbacks, &state, &exporter) != OTEL_STATUS_OK) {
        fprintf(stderr, "failed to create exporter: %s\n", otel_last_error_message().ptr);
        return 1;
    }

    /* A simple processor exports on the emitting thread, which keeps the output ordered. */
    if (otel_simple_span_processor_create(exporter, &processor) != OTEL_STATUS_OK) {
        fprintf(stderr, "failed to create processor\n");
        otel_trace_exporter_destroy(exporter);
        return 1;
    }

    builder = otel_sdk_builder_new();
    otel_sdk_builder_set_service_name(builder, otel_cstr("custom-trace-exporter-example"));
    if (otel_sdk_builder_add_span_processor(builder, processor) != OTEL_STATUS_OK) {
        fprintf(stderr, "failed to add span processor\n");
        otel_span_processor_destroy(processor);
        otel_sdk_builder_destroy(builder);
        return 1;
    }
    if (otel_sdk_build(builder, &sdk) != OTEL_STATUS_OK) {
        fprintf(stderr, "failed to build SDK\n");
        otel_sdk_builder_destroy(builder);
        return 1;
    }
    otel_sdk_builder_destroy(builder);

    /* Emit spans through the SDK's own provider so the pipeline is released on destroy. */
    otel_tracer_provider_t* provider = otel_sdk_get_tracer_provider(sdk);
    do_instrumentation_work(provider);
    otel_tracer_provider_destroy(provider);

    otel_sdk_force_flush(sdk, 5000);
    otel_sdk_shutdown(sdk, 5000);
    otel_sdk_destroy(sdk);
    return 0;
}
