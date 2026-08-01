/*
 * cpp_header_consumer/main.cpp
 *
 * C++17 consumer that includes the OpenTelemetry C API headers inside extern "C"
 * and exercises the C API from C++. Verifies that the headers are C++-compatible
 * and that the library links correctly from a C++ translation unit.
 */
#include <opentelemetry_c/api.h>

#include <cstdio>
#include <cstdlib>
#include <string_view>

int main() {
    otel_string_view_t version = otel_version_string();
    std::string_view sv{version.ptr, static_cast<std::size_t>(version.len)};
    std::printf("opentelemetry-c-api version (C++17): %.*s\n",
                static_cast<int>(sv.size()), sv.data());

    /* Verify API is callable from C++ (no-op without SDK). */
    otel_tracer_provider_t *provider = otel_global_tracer_provider();
    otel_tracer_t *tracer = otel_tracer_provider_get_tracer(
        provider,
        otel_cstr("cpp-consumer-test"),
        otel_cstr("0.1.0"),
        otel_string_view_empty());

    otel_span_start_options_t opts{};
    opts.kind = OTEL_SPAN_KIND_INTERNAL;
    opts.parent = nullptr;
    otel_span_t *span = otel_tracer_start_span(tracer, otel_cstr("cpp-test-span"), &opts);
    otel_span_set_ok(span);
    otel_span_end(span);
    otel_span_destroy(span);

    otel_tracer_destroy(tracer);
    otel_tracer_provider_destroy(provider);

    std::printf("cpp_consumer: OK\n");
    return EXIT_SUCCESS;
}
