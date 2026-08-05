/*
 * opentelemetry_c/otlp_trace_exporter.h
 *
 * OTLP Traces exporter builder (HTTP/protobuf and gRPC), with explicit
 * transport/compression selection. It produces a generic otel_trace_exporter_t (see
 * trace_exporter.h) that a span processor builder then consumes.
 *
 * The exporter owns its own blocking HTTP client, so no user-managed async runtime is
 * required. HTTPS is available via a selectable TLS backend chosen at compile time with the
 * crate's cargo features: `native-tls` (default; the platform TLS stack) or `rustls-tls`.
 * The gRPC transport owns a private single-worker Tokio runtime per exporter. HTTP endpoints
 * normally include "/v1/traces"; gRPC endpoints normally contain only scheme and authority.
 *
 * Part of `libopentelemetry_c_sdk`. Requires linking the SDK alongside the API.
 */
#ifndef OPENTELEMETRY_C_OTLP_TRACE_EXPORTER_H
#define OPENTELEMETRY_C_OTLP_TRACE_EXPORTER_H

#include <opentelemetry_c/common.h>
#include <opentelemetry_c/otlp_metric_exporter.h> /* otel_otlp_compression_t */
#include <opentelemetry_c/trace_exporter.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Opaque OTLP trace exporter builder. Not thread-safe; confine to one thread. */
typedef struct otel_otlp_trace_exporter_builder_t otel_otlp_trace_exporter_builder_t;

typedef uint32_t otel_otlp_trace_transport_t;
enum {
    /* Default when no protocol environment variable is set. */
    OTEL_OTLP_TRACE_TRANSPORT_HTTP_PROTOBUF = 0,
    /* Endpoint is normally an authority, e.g. http://localhost:4317. */
    OTEL_OTLP_TRACE_TRANSPORT_GRPC = 1
};

/* Create a new OTLP trace exporter builder. NULL only on allocation failure. Release with
 * otel_otlp_trace_exporter_builder_destroy(). */
otel_otlp_trace_exporter_builder_t* otel_otlp_trace_exporter_builder_new(void);

/* Destroy an OTLP trace exporter builder (no-op on NULL). */
void otel_otlp_trace_exporter_builder_destroy(otel_otlp_trace_exporter_builder_t* builder);

/*
 * Set the full OTLP traces endpoint URL, used as-is (no path is appended), e.g.
 * "http://localhost:4318/v1/traces". Remember to include the "/v1/traces" path.
 *
 * If unset, the exporter falls back to (in order): the
 * OTEL_EXPORTER_OTLP_TRACES_ENDPOINT environment variable (used as-is), the
 * OTEL_EXPORTER_OTLP_ENDPOINT environment variable (with "/v1/traces" appended), and
 * finally the OTLP default "http://localhost:4318/v1/traces". Programmatic configuration
 * takes precedence over the environment variables.
 */
otel_status_t otel_otlp_trace_exporter_builder_set_endpoint(
    otel_otlp_trace_exporter_builder_t* builder, otel_string_view_t endpoint);

/*
 * Select the transport explicitly. Without this call, the builder reads
 * OTEL_EXPORTER_OTLP_TRACES_PROTOCOL, then OTEL_EXPORTER_OTLP_PROTOCOL, and finally defaults
 * to HTTP/protobuf. The requested transport must be compiled into the SDK or build returns
 * OTEL_STATUS_INVALID_CONFIG. Transport is never inferred from endpoint syntax.
 */
otel_status_t otel_otlp_trace_exporter_builder_set_transport(
    otel_otlp_trace_exporter_builder_t* builder, otel_otlp_trace_transport_t transport);

/*
 * Select compression, reusing otel_otlp_compression_t. Gzip/zstd require the matching Cargo
 * feature for the selected transport; build fails rather than silently disabling unavailable
 * compression. NONE leaves compression unset so upstream environment/default resolution
 * still applies.
 */
otel_status_t otel_otlp_trace_exporter_builder_set_compression(
    otel_otlp_trace_exporter_builder_t* builder, otel_otlp_compression_t compression);

/*
 * Add a header (HTTP) / metadata entry (gRPC) sent with every export request.
 *
 * Duplicate keys are rejected case-insensitively: adding a key that matches an already-added
 * key under ASCII case-insensitive comparison (e.g. "Authorization" vs "authorization")
 * returns OTEL_STATUS_INVALID_ARGUMENT (with a message via otel_last_error_message()) and
 * leaves the builder unchanged, rather than silently overwriting the earlier value.
 */
otel_status_t otel_otlp_trace_exporter_builder_add_header(
    otel_otlp_trace_exporter_builder_t* builder, otel_string_view_t key,
    otel_string_view_t value);

/* Set the per-request export timeout in milliseconds. Zero leaves it unset so
 * OTEL_EXPORTER_OTLP_TRACES_TIMEOUT, OTEL_EXPORTER_OTLP_TIMEOUT, and then the exporter default
 * apply. */
otel_status_t otel_otlp_trace_exporter_builder_set_timeout_millis(
    otel_otlp_trace_exporter_builder_t* builder, uint64_t timeout_millis);

/*
 * Build a trace exporter from the accumulated configuration. On OTEL_STATUS_OK writes a new
 * otel_trace_exporter_t handle to *out (owned by the caller) and returns OTEL_STATUS_OK. On
 * failure sets *out to NULL, returns an error status, and records a message retrievable via
 * otel_last_error_message(). The builder is not consumed and must still be destroyed.
 *
 * Ownership of *out: release it with otel_trace_exporter_destroy(), or transfer it into a
 * span processor builder via otel_batch_span_processor_builder_set_exporter().
 *
 * The requested transport must be compiled into the SDK; otherwise this function returns
 * OTEL_STATUS_INVALID_CONFIG, writes NULL to *out, and records a last-error message naming
 * the required cargo feature.
 */
otel_status_t otel_otlp_trace_exporter_builder_build(
    const otel_otlp_trace_exporter_builder_t* builder, otel_trace_exporter_t** out);

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* OPENTELEMETRY_C_OTLP_TRACE_EXPORTER_H */
