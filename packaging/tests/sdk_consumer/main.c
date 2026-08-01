/*
 * sdk_consumer/main.c
 *
 * Minimal C11 program that builds a transport-free SDK pipeline and exercises the
 * full API+SDK path. Built with --no-default-features so no network/TLS is needed.
 * Used by packaging/CI tests to verify that the installed SDK headers and
 * libopentelemetry_c_sdk link and run correctly.
 */
#include <opentelemetry_c/api.h>
#include <opentelemetry_c/sdk.h>

#include <stdio.h>
#include <stdlib.h>

int main(void) {
    otel_string_view_t version = otel_version_string();
    printf("opentelemetry-c-api version: %.*s\n", (int)version.len, version.ptr);

    /* Build a minimal SDK without any exporters (no-default-features build). */
    otel_sdk_builder_t *builder = otel_sdk_builder_new();
    if (!builder) {
        fprintf(stderr, "sdk_consumer: otel_sdk_builder_new failed\n");
        return EXIT_FAILURE;
    }

    otel_sdk_builder_set_service_name(builder, otel_cstr("sdk-consumer-test"));

    otel_sdk_t *sdk = NULL;
    otel_status_t st = otel_sdk_build(builder, &sdk);
    otel_sdk_builder_destroy(builder);

    if (st != OTEL_STATUS_OK || sdk == NULL) {
        otel_string_view_t msg = otel_last_error_message();
        fprintf(stderr, "sdk_consumer: otel_sdk_build failed: %.*s\n",
                (int)msg.len, msg.ptr);
        return EXIT_FAILURE;
    }

    st = otel_sdk_set_as_global(sdk);
    if (st != OTEL_STATUS_OK) {
        otel_string_view_t msg = otel_last_error_message();
        fprintf(stderr, "sdk_consumer: otel_sdk_set_as_global failed: %.*s\n",
                (int)msg.len, msg.ptr);
        otel_sdk_destroy(sdk);
        return EXIT_FAILURE;
    }

    /* Emit a span via the API (resolves through the installed SDK). */
    otel_tracer_provider_t *provider = otel_global_tracer_provider();
    otel_tracer_t *tracer = otel_tracer_provider_get_tracer(
        provider,
        otel_cstr("sdk-consumer-test"),
        otel_cstr("0.1.0"),
        otel_string_view_empty());

    otel_span_start_options_t opts;
    opts.kind = OTEL_SPAN_KIND_INTERNAL;
    opts.parent = NULL;
    otel_span_t *span = otel_tracer_start_span(tracer, otel_cstr("sdk-test-span"), &opts);
    otel_span_set_string_attribute(span, otel_cstr("sdk.test"), otel_cstr("true"));
    otel_span_set_ok(span);
    otel_span_end(span);
    otel_span_destroy(span);

    otel_tracer_destroy(tracer);
    otel_tracer_provider_destroy(provider);

    otel_sdk_shutdown(sdk, 2000);
    otel_sdk_destroy(sdk);

    printf("sdk_consumer: OK\n");
    return EXIT_SUCCESS;
}
