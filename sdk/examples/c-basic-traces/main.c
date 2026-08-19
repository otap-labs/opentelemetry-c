// SPDX-License-Identifier: Apache-2.0

/*
 * c-basic-traces: a C program demonstrating the split API/SDK model.
 *
 * The APPLICATION role (this file) builds and installs the SDK. Trace calls then go
 * through the API library's global provider — exactly as an API-only instrumentation
 * library would make them. This proves that spans created via the API-only path are
 * exported by the SDK installed into the API-owned global slot.
 *
 * Links against BOTH libopentelemetry_c_api and libopentelemetry_c_sdk. Exports to
 * http://localhost:4318/v1/traces by default (override with
 * OTEL_EXPORTER_OTLP_TRACES_ENDPOINT). See the Makefile and README.md.
 */
#include <opentelemetry_c/api.h> /* API: common.h + trace.h */
#include <opentelemetry_c/sdk.h> /* SDK: builder + lifecycle */
#include <opentelemetry_c/otlp_trace_exporter.h> /* OTLP HTTP/protobuf exporter builder */
#include <opentelemetry_c/batch_span_processor.h> /* batch span processor builder */

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static void print_last_error(const char* context) {
    otel_string_view_t msg = otel_last_error_message();
    if (msg.ptr != NULL && msg.len > 0) {
        fprintf(stderr, "%s: %.*s\n", context, (int)msg.len, msg.ptr);
    } else {
        fprintf(stderr, "%s: (no detail)\n", context);
    }
}

/* Parent a child from the API-owned context attached to the current thread. */
static void emit_ambient_parent_child(otel_tracer_t* tracer) {
    otel_span_start_options_t parent_opts = {OTEL_SPAN_KIND_SERVER, NULL};
    otel_span_t* parent =
        otel_tracer_start_span(tracer, otel_cstr("ambient-request"), &parent_opts);
    otel_span_context_t* parent_span_context = NULL;
    otel_context_t* parent_context = NULL;
    otel_context_scope_t scope = OTEL_CONTEXT_SCOPE_INIT;
    otel_span_t* child = NULL;

    if (parent == NULL ||
        otel_span_get_context(parent, &parent_span_context) != OTEL_STATUS_OK ||
        (parent_context = otel_context_create(parent_span_context)) == NULL ||
        otel_context_attach(parent_context, &scope) != OTEL_STATUS_OK) {
        print_last_error("ambient parent setup failed");
        goto cleanup;
    }

    otel_span_start_options_ex_t child_opts = OTEL_SPAN_START_OPTIONS_EX_INIT;
    child_opts.kind = OTEL_SPAN_KIND_INTERNAL;
    child_opts.parent_mode = OTEL_PARENT_AMBIENT;
    child = otel_tracer_start_span_ex(tracer, otel_cstr("ambient-child"), &child_opts);

    /* Starting the child captures its parent; the ambient scope can now be detached. */
    if (otel_context_scope_detach(&scope) != OTEL_STATUS_OK) {
        print_last_error("ambient context detach failed");
    }
    if (child == NULL) {
        print_last_error("ambient child start failed");
        goto cleanup;
    }

    otel_span_set_ok(child);
    otel_span_end(child);

cleanup:
    otel_span_destroy(child);
    otel_context_destroy(parent_context);
    otel_span_context_destroy(parent_span_context);
    if (parent != NULL) {
        otel_span_set_ok(parent);
        otel_span_end(parent);
    }
    otel_span_destroy(parent);
}

/* Simulate sending traceparent over a carrier and receiving it in another process. */
static void emit_w3c_propagated_parent_child(otel_tracer_t* tracer) {
    otel_span_start_options_t producer_opts = {OTEL_SPAN_KIND_CLIENT, NULL};
    otel_span_t* producer =
        otel_tracer_start_span(tracer, otel_cstr("send-request"), &producer_opts);
    otel_span_context_t* producer_context = NULL;
    otel_span_context_t* remote_parent = NULL;
    otel_span_t* consumer = NULL;
    char traceparent[55];
    size_t traceparent_len = 0;

    if (producer == NULL ||
        otel_span_get_context(producer, &producer_context) != OTEL_STATUS_OK ||
        otel_trace_propagation_inject_traceparent(
            producer_context, traceparent, sizeof(traceparent), &traceparent_len) !=
            OTEL_STATUS_OK) {
        print_last_error("traceparent injection failed");
        goto cleanup;
    }

    printf("propagating traceparent: %.*s\n", (int)traceparent_len, traceparent);

    /* A real receiver reads these bytes from its transport's header carrier. */
    otel_string_view_t carrier_value = {traceparent, traceparent_len};
    if (otel_trace_propagation_extract(
            carrier_value, otel_string_view_empty(), &remote_parent) != OTEL_STATUS_OK) {
        print_last_error("traceparent extraction failed");
        goto cleanup;
    }

    otel_span_start_options_t consumer_opts = {OTEL_SPAN_KIND_SERVER, NULL};
    consumer = otel_tracer_start_span_with_context(
        tracer, otel_cstr("receive-request"), &consumer_opts, remote_parent);
    if (consumer == NULL) {
        print_last_error("remote child start failed");
        goto cleanup;
    }

    otel_span_set_ok(consumer);
    otel_span_end(consumer);

cleanup:
    otel_span_destroy(consumer);
    otel_span_context_destroy(remote_parent);
    otel_span_context_destroy(producer_context);
    if (producer != NULL) {
        otel_span_set_ok(producer);
        otel_span_end(producer);
    }
    otel_span_destroy(producer);
}

/* Emit spans using ONLY the API (as an instrumentation library would). */
static void do_instrumentation_work(void) {
    otel_tracer_provider_t* provider = otel_global_tracer_provider();
    otel_tracer_t* tracer = otel_tracer_provider_get_tracer(
        provider, otel_cstr("c-basic-traces"), otel_cstr("0.1.0"), otel_string_view_empty());

    otel_span_start_options_t parent_opts;
    parent_opts.kind = OTEL_SPAN_KIND_SERVER;
    parent_opts.parent = NULL;
    otel_span_t* parent = otel_tracer_start_span(tracer, otel_cstr("handle-request"), &parent_opts);
    otel_span_set_string_attribute(parent, otel_cstr("http.request.method"), otel_cstr("GET"));
    otel_span_set_int64_attribute(parent, otel_cstr("http.response.status_code"), 200);

    otel_span_start_options_t child_opts;
    child_opts.kind = OTEL_SPAN_KIND_CLIENT;
    child_opts.parent = parent; /* fallback used by the API-only no-op path */
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

    if (otel_tracer_supports_context(tracer) == OTEL_TRUE) {
        emit_ambient_parent_child(tracer);
        emit_w3c_propagated_parent_child(tracer);
    }

    otel_tracer_destroy(tracer);
    otel_tracer_provider_destroy(provider);
}

int main(void) {
    otel_string_view_t version = otel_version_string();
    printf("opentelemetry-c-api version %.*s\n", (int)version.len, version.ptr);

    /* Before any SDK: API-only calls are safe no-ops. */
    do_instrumentation_work();
    printf("api-only no-op path OK\n");

    /* Application: build the trace pipeline (exporter -> processor -> SDK) and install it. */
    const char* endpoint = getenv("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT");
    if (endpoint == NULL || endpoint[0] == '\0') {
        endpoint = "http://localhost:4318/v1/traces";
    }
    printf("exporting to %s\n", endpoint);

    /* 1. OTLP HTTP/protobuf trace exporter. */
    otel_otlp_trace_exporter_builder_t* eb = otel_otlp_trace_exporter_builder_new();
    otel_otlp_trace_exporter_builder_set_endpoint(eb, otel_cstr(endpoint));
    otel_otlp_trace_exporter_builder_set_timeout_millis(eb, 10000);
    otel_trace_exporter_t* exporter = NULL;
    if (otel_otlp_trace_exporter_builder_build(eb, &exporter) != OTEL_STATUS_OK || exporter == NULL) {
        print_last_error("otel_otlp_trace_exporter_builder_build failed");
        otel_otlp_trace_exporter_builder_destroy(eb);
        return EXIT_FAILURE;
    }
    otel_otlp_trace_exporter_builder_destroy(eb);

    /* 2. Batch span processor wrapping the exporter (ownership transfers on OK). */
    otel_batch_span_processor_builder_t* pb = otel_batch_span_processor_builder_new();
    if (otel_batch_span_processor_builder_set_exporter(pb, exporter) != OTEL_STATUS_OK) {
        print_last_error("otel_batch_span_processor_builder_set_exporter failed");
        otel_trace_exporter_destroy(exporter); /* still ours on failure */
        otel_batch_span_processor_builder_destroy(pb);
        return EXIT_FAILURE;
    }
    otel_batch_span_processor_builder_set_max_queue_size(pb, 2048);
    otel_span_processor_t* processor = NULL;
    if (otel_batch_span_processor_builder_build(pb, &processor) != OTEL_STATUS_OK || processor == NULL) {
        print_last_error("otel_batch_span_processor_builder_build failed");
        otel_batch_span_processor_builder_destroy(pb);
        return EXIT_FAILURE;
    }
    otel_batch_span_processor_builder_destroy(pb);

    /* 3. SDK builder with the processor (ownership transfers on OK). */
    otel_sdk_builder_t* builder = otel_sdk_builder_new();
    otel_sdk_builder_set_service_name(builder, otel_cstr("c-basic-traces"));
    if (otel_sdk_builder_add_span_processor(builder, processor) != OTEL_STATUS_OK) {
        print_last_error("otel_sdk_builder_add_span_processor failed");
        otel_span_processor_destroy(processor); /* still ours on failure */
        otel_sdk_builder_destroy(builder);
        return EXIT_FAILURE;
    }

    otel_sdk_t* sdk = NULL;
    otel_status_t status = otel_sdk_build(builder, &sdk);
    otel_sdk_builder_destroy(builder);
    if (status != OTEL_STATUS_OK || sdk == NULL) {
        print_last_error("otel_sdk_build failed");
        return EXIT_FAILURE;
    }
    if (otel_sdk_set_as_global(sdk) != OTEL_STATUS_OK) {
        print_last_error("otel_sdk_set_as_global failed");
        otel_sdk_destroy(sdk);
        return EXIT_FAILURE;
    }

    /* Instrumentation work now flows through the installed SDK, via the API only. */
    do_instrumentation_work();
    printf("api-only spans emitted through installed SDK\n");

    otel_sdk_force_flush(sdk, 5000);
    otel_sdk_shutdown(sdk, 5000);
    otel_sdk_destroy(sdk);
    printf("done\n");
    return EXIT_SUCCESS;
}
