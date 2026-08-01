/*
 * Conan test_package: minimal C program that includes the OpenTelemetry C API
 * header and calls a version query to confirm the library loaded correctly.
 */
#include <opentelemetry_c/api.h>
#include <stdio.h>

int main(void) {
    otel_string_view_t v = otel_version_string();
    printf("opentelemetry-c version: %.*s\n", (int)v.len, v.ptr);
    return 0;
}
