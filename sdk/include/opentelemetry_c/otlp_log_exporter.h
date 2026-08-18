// SPDX-License-Identifier: Apache-2.0

/*
 * opentelemetry_c/otlp_log_exporter.h
 *
 * OTLP Logs exporter builder (HTTP/protobuf and gRPC).
 *
 * EXPERIMENTAL: the Logs C API is experimental and may change in a future release.
 *
 * Part of `libopentelemetry_c_sdk`. Requires linking the SDK alongside the API.
 */
#ifndef OPENTELEMETRY_C_OTLP_LOG_EXPORTER_H
#define OPENTELEMETRY_C_OTLP_LOG_EXPORTER_H

#include <opentelemetry_c/common.h>
#include <opentelemetry_c/log_exporter.h>
#include <opentelemetry_c/otlp_metric_exporter.h> /* otel_otlp_compression_t */

#ifdef __cplusplus
extern "C" {
#endif

typedef struct otel_otlp_log_exporter_builder_t otel_otlp_log_exporter_builder_t;

typedef uint32_t otel_otlp_log_transport_t;
enum {
    /* Default. Endpoint normally includes the signal path, e.g. /v1/logs. */
    OTEL_OTLP_LOG_TRANSPORT_HTTP_PROTOBUF = 0,
    /* Endpoint is normally an authority, e.g. http://localhost:4317. */
    OTEL_OTLP_LOG_TRANSPORT_GRPC = 1
};

/* Create an OTLP Logs exporter builder. NULL only on allocation failure. A builder is NOT
 * thread-safe; confine it to a single thread. */
otel_otlp_log_exporter_builder_t* otel_otlp_log_exporter_builder_new(void);
void otel_otlp_log_exporter_builder_destroy(otel_otlp_log_exporter_builder_t* builder);

/*
 * A programmatic endpoint overrides the upstream OTLP environment endpoint. HTTP endpoints
 * normally include /v1/logs; gRPC endpoints normally contain only scheme and authority. The
 * endpoint is used as-is and is never rewritten from the selected transport.
 */
otel_status_t otel_otlp_log_exporter_builder_set_endpoint(
    otel_otlp_log_exporter_builder_t* builder, otel_string_view_t endpoint);

/*
 * Select the transport explicitly; HTTP/protobuf is the default. The requested transport must
 * be compiled into the SDK or build returns OTEL_STATUS_INVALID_CONFIG. Transport is never
 * inferred from endpoint syntax.
 */
otel_status_t otel_otlp_log_exporter_builder_set_transport(
    otel_otlp_log_exporter_builder_t* builder, otel_otlp_log_transport_t transport);

/*
 * Select compression, reusing otel_otlp_compression_t. Gzip/zstd require the matching Cargo
 * feature for the selected transport; build fails rather than silently disabling unavailable
 * compression. NONE leaves compression unset so upstream environment/default resolution
 * still applies.
 */
otel_status_t otel_otlp_log_exporter_builder_set_compression(
    otel_otlp_log_exporter_builder_t* builder, otel_otlp_compression_t compression);

/*
 * For HTTP these are request headers. For gRPC they are validated ASCII metadata. Binary
 * metadata keys ending in "-bin" are unsupported; arbitrary strings are not reinterpreted as
 * binary values. Keys must be non-empty and unique case-insensitively, so a later value never
 * silently replaces an earlier one. Diagnostics name invalid keys but NEVER include header
 * values, which routinely carry credentials.
 */
otel_status_t otel_otlp_log_exporter_builder_add_header(
    otel_otlp_log_exporter_builder_t* builder,
    otel_string_view_t key, otel_string_view_t value);

/* Per-request timeout in milliseconds (0 == exporter default). */
otel_status_t otel_otlp_log_exporter_builder_set_timeout_millis(
    otel_otlp_log_exporter_builder_t* builder, uint64_t timeout_millis);

/*
 * Build an owned exporter. On OTEL_STATUS_OK *out receives a new otel_log_exporter_t; on
 * failure *out is set to NULL. The builder is not consumed and may be built again.
 *
 * The gRPC transport creates and owns a private single-worker Tokio runtime per exporter;
 * exports run synchronously on the processor thread by blocking on that runtime. Building
 * gRPC from inside an already-entered Tokio runtime is supported, but exporting from one is
 * rejected with an internal-failure export error rather than panicking.
 */
otel_status_t otel_otlp_log_exporter_builder_build(
    const otel_otlp_log_exporter_builder_t* builder, otel_log_exporter_t** out);

#ifdef __cplusplus
}
#endif
#endif
