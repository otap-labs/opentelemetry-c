// SPDX-License-Identifier: Apache-2.0

#ifndef OPENTELEMETRY_C_OTLP_METRIC_EXPORTER_H
#define OPENTELEMETRY_C_OTLP_METRIC_EXPORTER_H

#include <opentelemetry_c/common.h>
#include <opentelemetry_c/metric_exporter.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct otel_otlp_metric_exporter_builder_t otel_otlp_metric_exporter_builder_t;

typedef uint32_t otel_otlp_metric_transport_t;
enum {
    /* Default. Endpoint normally includes the signal path, e.g. /v1/metrics. */
    OTEL_OTLP_METRIC_TRANSPORT_HTTP_PROTOBUF = 0,
    /* Endpoint is normally an authority, e.g. http://localhost:4317. */
    OTEL_OTLP_METRIC_TRANSPORT_GRPC = 1
};

typedef uint32_t otel_otlp_compression_t;
enum {
    OTEL_OTLP_COMPRESSION_NONE = 0,
    OTEL_OTLP_COMPRESSION_GZIP = 1,
    OTEL_OTLP_COMPRESSION_ZSTD = 2
};

otel_otlp_metric_exporter_builder_t* otel_otlp_metric_exporter_builder_new(void);
void otel_otlp_metric_exporter_builder_destroy(otel_otlp_metric_exporter_builder_t* builder);
/*
 * A programmatic endpoint overrides the upstream OTLP environment endpoint. HTTP endpoints
 * normally include /v1/metrics; gRPC endpoints normally contain only scheme and authority.
 */
otel_status_t otel_otlp_metric_exporter_builder_set_endpoint(
    otel_otlp_metric_exporter_builder_t* builder, otel_string_view_t endpoint);
/*
 * Select the transport explicitly; HTTP/protobuf is the default. The requested transport
 * must be compiled into the SDK or build returns OTEL_STATUS_INVALID_CONFIG. Transport is
 * never inferred from endpoint syntax.
 */
otel_status_t otel_otlp_metric_exporter_builder_set_transport(
    otel_otlp_metric_exporter_builder_t* builder, otel_otlp_metric_transport_t transport);
/*
 * Select compression. Gzip/zstd require the matching Cargo feature for the selected
 * transport; build fails rather than silently disabling unavailable compression.
 * NONE leaves compression unset so upstream environment/default resolution still applies.
 */
otel_status_t otel_otlp_metric_exporter_builder_set_compression(
    otel_otlp_metric_exporter_builder_t* builder, otel_otlp_compression_t compression);
/*
 * For HTTP these are request headers. For gRPC they are validated ASCII metadata. Binary
 * metadata keys ending in "-bin" are unsupported; arbitrary strings are not reinterpreted
 * as binary values. Diagnostics name invalid keys but never include metadata values.
 */
otel_status_t otel_otlp_metric_exporter_builder_add_header(
    otel_otlp_metric_exporter_builder_t* builder,
    otel_string_view_t key, otel_string_view_t value);
otel_status_t otel_otlp_metric_exporter_builder_set_timeout_millis(
    otel_otlp_metric_exporter_builder_t* builder, uint64_t timeout_millis);
otel_status_t otel_otlp_metric_exporter_builder_set_temporality(
    otel_otlp_metric_exporter_builder_t* builder, otel_metric_temporality_t temporality);
otel_status_t otel_otlp_metric_exporter_builder_build(
    const otel_otlp_metric_exporter_builder_t* builder, otel_metric_exporter_t** out);

#ifdef __cplusplus
}
#endif
#endif
