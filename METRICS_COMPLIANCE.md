# Experimental Metrics compliance

This ledger records the implemented OpenTelemetry Metrics surface for the current
experimental `0.x` C ABI.

| Area | Status | Notes |
| --- | --- | --- |
| API-only operation | Implemented | Independent API-owned global `MeterProvider`; no SDK dependency and safe no-op instruments before installation. |
| Synchronous instruments | Implemented | `u64`/`f64` counters, `i64`/`f64` up-down counters, `u64`/`i64`/`f64` gauges, and `u64`/`f64` histograms. |
| Observable instruments | Implemented | Counters, up-down counters, and gauges for all Rust SDK-supported numeric types; callback user data has exactly-once destruction independent of upstream closure release. |
| Observer lifetime | Implemented | Observer tokens are valid only on the callback thread until return; stale and cross-thread use fail closed. Dispatch is thread-local, so independent readers are not serialized by an API-global lock. Destroying the public instrument disables future callback work. |
| Instrument validation | Implemented | Name, unit, UTF-8, options structure size, and explicit histogram boundary validation occurs before SDK dispatch. |
| Instrumentation scope | Implemented | Versioned meter options carry name, version, schema URL, and uniquely keyed typed attributes into the upstream owned `InstrumentationScope`. |
| SDK pipeline | Implemented | Independent `SdkMeterProvider`, multiple periodic/manual readers, resource/scope propagation, force flush, and shutdown. |
| Custom export | Implemented | C callback-backed push exporter with complete resource/scope/metric/point/exemplar visitation, scalar/array attributes, exact callback-state destruction, and callback-scoped batch tokens. |
| Manual collection | Implemented | Worker-free manual reader; `otel_sdk_metrics_force_flush` performs one synchronous collection/export cycle on the caller thread. |
| Async periodic collection | Implemented | Optional SDK-owned single-worker Tokio runtime, upstream cooperative export-timeout mapping, multiple-reader support, and deterministic runtime disposal. Blocking remains the default. |
| OTLP Metrics | Implemented | HTTP/protobuf by default plus optional gRPC/tonic, explicit transport selection, endpoint, headers/ASCII metadata, timeout, transport-specific compression, and cumulative/delta/low-memory temporality preference. |
| Views | Implemented | Exact or single-wildcard name selection, scope name/version/schema/required-attribute selection, unit/kind selection, stream metadata, attribute allow-list, cardinality limit, and all supported aggregations. |
| Split-artifact linking | Implemented | C integration links separate API/SDK shared libraries and verifies OTLP Metrics bytes through the API-owned global slot. |
| C and C++ headers | Implemented | All Metrics headers compile standalone as C11; the combined pipeline headers compile as C++17. |
| Hot path | Implemented | SDK-backed synchronous handles own concrete Rust instruments; recording does not resolve providers, lock global state, or access readers/exporters/views. |
| ABI compatibility | Implemented | Separate Metrics vtable with prefix-only version/size checks plus a common opaque-handle kind prefix validated before full typed access. |
| Status/error policy | Implemented | Malformed arguments, incompatible configuration/ABI, timeout, export-pipeline failure, and internal infrastructure failure have signal-independent classifications and last-error diagnostics. |

## Known experimental constraints

- Metrics are experimental and may change incompatibly between `0.x` releases.
- The default Rust 0.32 blocking periodic reader controls collection on its worker thread; its
  force flush may block, and its shutdown uses the upstream reader's fixed timeout behavior.
- The gRPC exporter owns one bounded Tokio runtime for its complete reader/provider lifetime.
  C callers do not supply a runtime. Its synchronous runtime wrapper is incompatible with the
  optional async periodic reader and is rejected during reader construction.
- The async reader's upstream timeout is cooperative and cannot preempt the current synchronous
  HTTP exporter or a custom C callback. HTTP callers should also configure the exporter transport
  timeout; custom callbacks must return promptly.
- gRPC binary `-bin` metadata and custom certificate/key configuration are not exposed.
  HTTPS requires the opt-in `grpc-tls-ring` Cargo feature, which uses upstream tonic TLS with
  native/platform roots.
- Observable callbacks cannot be unregistered from the upstream Rust SDK. Destroying the C
  handle disables callback work and releases user data after any in-flight callback. Metrics
  shutdown/destroy also removes the SDK's global provider registration when still current.
  Observer tokens cannot be handed to another thread, including work spawned by a callback.
- The supported shared-global deployment model is one shared API library on Linux or macOS,
  loaded before the matching SDK and retained for process lifetime after use. Windows
  shared-library use and static deployment are unsupported.
- Arbitrary third-party reader vtables are not exposed. The pinned upstream experimental
  reader trait is enabled only to implement the supported worker-free manual reader.
- Logs and asynchronous user-controlled collection are not exposed.
