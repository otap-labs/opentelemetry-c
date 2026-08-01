/*
 * api_consumer/main.c
 *
 * Minimal C11 program that exercises the OpenTelemetry C API without an SDK.
 * All trace calls are safe no-ops when no SDK is installed.
 * Used by packaging/CI tests to verify that the installed API-only headers and
 * libopentelemetry_c_api link correctly.
 */
#include <opentelemetry_c/api.h>

#include <stdio.h>
#include <stdlib.h>

int main(void) {
    /* Print the library version to confirm the library loaded. */
    otel_string_view_t version = otel_version_string();
    printf("opentelemetry-c-api version: %.*s\n", (int)version.len, version.ptr);

    /* Without an SDK installed, the global provider is a no-op. These calls must
     * succeed without crashing. */
    otel_tracer_provider_t *provider = otel_global_tracer_provider();
    otel_tracer_t *tracer = otel_tracer_provider_get_tracer(
        provider,
        otel_cstr("api-consumer-test"),
        otel_cstr("0.1.0"),
        otel_string_view_empty());

    otel_span_start_options_t opts;
    opts.kind = OTEL_SPAN_KIND_INTERNAL;
    opts.parent = NULL;
    otel_span_t *span = otel_tracer_start_span(tracer, otel_cstr("test-span"), &opts);
    otel_span_set_string_attribute(span, otel_cstr("test.key"), otel_cstr("test-value"));
    otel_span_set_ok(span);
    otel_span_end(span);
    otel_span_destroy(span);

    otel_tracer_destroy(tracer);
    otel_tracer_provider_destroy(provider);

    printf("api_consumer: OK\n");
    return EXIT_SUCCESS;
}
