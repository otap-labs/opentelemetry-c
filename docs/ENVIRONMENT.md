# Environment Configuration

The C SDK honors standard OpenTelemetry environment variables during builder construction and
`otel_sdk_build()`. Environment lookup is setup-time work and adds no cost to span, metric, or
log recording.

Programmatic configuration takes precedence. For example, an explicit exporter endpoint or
transport overrides its corresponding environment variable, and
`otel_sdk_builder_set_disabled()` overrides `OTEL_SDK_DISABLED`. For the C-owned disable and
protocol selectors, empty values are treated as unset, invalid booleans and protocol names
produce a warning, and resolution falls back safely. Invalid numeric values handled by the
pinned Rust SDK are ignored.

## SDK and resource

| Variable | Status | Resolution |
| --- | --- | --- |
| `OTEL_SDK_DISABLED` | Supported | Read by `otel_sdk_build()`. `true` creates valid no-export providers; other valid values leave the SDK enabled. |
| `OTEL_SERVICE_NAME` | Supported | Applied by the upstream environment resource detector. |
| `OTEL_RESOURCE_ATTRIBUTES` | Supported | Parsed by the upstream environment resource detector. `OTEL_SERVICE_NAME` wins for `service.name`. |

Attributes added through `otel_sdk_builder_add_resource_attribute()` override environment
resource attributes with the same key. The dedicated programmatic service-name setter has the
highest `service.name` precedence.

## Traces and processors

| Variable | Status | Notes |
| --- | --- | --- |
| `OTEL_TRACES_SAMPLER` | Supported | `always_on`, `always_off`, `traceidratio`, `parentbased_always_on`, `parentbased_always_off`, and `parentbased_traceidratio`. |
| `OTEL_TRACES_SAMPLER_ARG` | Supported | Ratio for the ratio-based samplers. |
| `OTEL_BSP_MAX_QUEUE_SIZE` | Supported | Explicit batch-processor setter wins. |
| `OTEL_BSP_MAX_EXPORT_BATCH_SIZE` | Supported | Explicit batch-processor setter wins. |
| `OTEL_BSP_SCHEDULE_DELAY` | Supported | Milliseconds; explicit setter wins. |
| `OTEL_BSP_EXPORT_TIMEOUT` | Limited | Parsed by the pinned upstream configuration, but its stable synchronous batch processor cannot enforce a per-export timeout. |

## Logs processors

| Variable | Status | Notes |
| --- | --- | --- |
| `OTEL_BLRP_MAX_QUEUE_SIZE` | Supported | Explicit batch-log-processor setter wins. |
| `OTEL_BLRP_MAX_EXPORT_BATCH_SIZE` | Supported | Explicit setter wins. |
| `OTEL_BLRP_SCHEDULE_DELAY` | Supported | Milliseconds; explicit setter wins. |
| `OTEL_BLRP_EXPORT_TIMEOUT` | Not supported | The pinned stable synchronous Rust log processor does not expose or apply this field. Use the OTLP exporter request timeout to bound transport operations. |

## Metrics reader

| Variable | Status | Notes |
| --- | --- | --- |
| `OTEL_METRIC_EXPORT_INTERVAL` | Supported | Milliseconds; explicit periodic-reader interval wins. |
| `OTEL_METRIC_EXPORT_TIMEOUT` | Async reader only | Supported by the optional async reader. The pinned blocking reader does not expose a cooperative export timeout. |

## OTLP exporters

Endpoint, protocol, timeout, and compression use this precedence:

1. Programmatic builder setting.
2. Signal-specific variable.
3. Generic OTLP variable.
4. Upstream default.

The following generic variables and their `TRACES`, `METRICS`, and `LOGS` signal-specific forms
are supported:

| Setting | Generic variable | Signal-specific example |
| --- | --- | --- |
| Endpoint | `OTEL_EXPORTER_OTLP_ENDPOINT` | `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT` |
| Protocol | `OTEL_EXPORTER_OTLP_PROTOCOL` | `OTEL_EXPORTER_OTLP_TRACES_PROTOCOL` |
| Headers | `OTEL_EXPORTER_OTLP_HEADERS` | `OTEL_EXPORTER_OTLP_TRACES_HEADERS` |
| Timeout | `OTEL_EXPORTER_OTLP_TIMEOUT` | `OTEL_EXPORTER_OTLP_TRACES_TIMEOUT` |
| Compression | `OTEL_EXPORTER_OTLP_COMPRESSION` | `OTEL_EXPORTER_OTLP_TRACES_COMPRESSION` |

Supported protocols are `http/protobuf` and `grpc`, subject to the SDK's compile-time Cargo
features. `http/json` is not compiled into this SDK. A valid environment protocol whose
transport is absent fails exporter construction with `OTEL_STATUS_INVALID_CONFIG`; it never
silently selects a different transport.

Header values may contain credentials and are never included in C SDK diagnostics. TLS
certificate, client-certificate, client-key, and gRPC `insecure` environment variables are not
implemented by the pinned OpenTelemetry Rust 0.32 exporters and are not claimed here.

The pinned exporter merges programmatic headers with environment headers. Avoid defining the
same header key in both places: in Rust 0.32, the environment value wins that collision even
though the general OpenTelemetry configuration rule gives programmatic configuration higher
precedence. This is a documented pinned-dependency limitation, not a C FFI behavior.

## Snapshot timing

Environment variables are read when the relevant upstream builder is created or when
`otel_sdk_build()` runs. Changing the process environment concurrently with SDK construction is
not supported. Once built, an SDK is immutable; change configuration by building and installing
a replacement SDK.
