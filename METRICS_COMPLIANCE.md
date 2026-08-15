<!-- SPDX-License-Identifier: Apache-2.0 -->

# Experimental Metrics compliance

This ledger records the implemented OpenTelemetry Metrics surface for the current
experimental `0.x` C ABI.

| Area | Status | Notes |
| --- | --- | --- |
| API-only operation | Implemented | Independent API-owned global `MeterProvider`; no SDK dependency and safe no-op instruments before installation. |
| Synchronous instruments | Implemented | `u64`/`f64` counters, `i64`/`f64` up-down counters, `u64`/`i64`/`f64` gauges, and `u64`/`f64` histograms. |
| Bound instruments | Implemented (experimental) | Pre-bound scalar attributes for `u64`/`f64` counters and histograms, matching the subset exposed by the pinned Rust 0.32 experimental API. Recording performs no per-call attribute conversion. |
| Observable instruments | Implemented | Counters, up-down counters, and gauges for all Rust SDK-supported numeric types; successful creation owns callback user data with exactly-once destruction, while failure preserves caller ownership. |
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
| Resource bounds | Implemented | SDK builders accept at most 64 span processors, 64 Metrics readers, 1024 views, and 1024 resource attributes; each view accepts at most 256 scope matchers and 1024 allowed attribute keys. Capacity is reserved before ownership transfer. |

## Known experimental constraints

- Metrics are experimental and may change incompatibly between `0.x` releases.
- Bound instruments follow the upstream `experimental_metrics_bound_instruments` API.
  OpenTelemetry Rust 0.32 exposes bound counters and histograms, but not bound gauges or
  up-down counters. Binding copies and pre-resolves attributes during setup; callers should
  reuse the bound handle only when that attribute set is stable. A handle bound while the
  stream cardinality limit is exhausted remains attached to the overflow series for its
  lifetime; drop and re-bind after capacity becomes available to resolve the original
  attribute set. As with unbound upstream recording, a poisoned internal tracker lock causes
  subsequent measurements to be discarded rather than panicking across the telemetry hot
  path.
- The default Rust 0.32 blocking periodic reader controls collection on its worker thread.
  Metrics force flush has no upstream timeout input and can block indefinitely if an exporter
  or collection callback does not return. The trace helper-thread timeout is not reused
  because an optional Metrics async runtime is owned outside the cloneable provider; a
  detached flush must not outlive deterministic runtime disposal during SDK destruction.
  Shutdown uses the upstream reader's fixed timeout behavior.
- The gRPC exporter owns one bounded Tokio runtime for its complete reader/provider lifetime.
  C callers do not supply a runtime. Its synchronous runtime wrapper is incompatible with the
  optional async periodic reader and is rejected during reader construction.
- The blocking OTLP/HTTP exporter is also incompatible with the async reader because reqwest's
  blocking client cannot run safely inside Tokio. The async reader currently supports custom
  exporters only.
- The async reader's upstream timeout is cooperative and cannot preempt a custom C callback;
  custom callbacks must return promptly.
- gRPC binary `-bin` metadata and custom certificate/key configuration are not exposed.
  HTTPS requires the opt-in `grpc-tls-ring` Cargo feature, which uses upstream tonic TLS with
  native/platform roots.
- OTLP Metrics exposes HTTP/protobuf and gRPC/tonic only; the upstream HTTP/JSON encoding is
  not part of the current C transport enum.
- `OTEL_METRIC_TEMPORALITY_DEFAULT` selects cumulative temporality for custom exporters. OTLP
  exporters instead defer the default preference to the upstream environment/configuration
  resolution.
- Exemplar visitor callbacks are wired for every supported metric data type, but the pinned
  Rust SDK 0.32.1 currently emits empty exemplar lists, so those callbacks are not invoked in
  practice.
- Observable callbacks cannot be unregistered from the upstream Rust SDK. Destroying the C
  handle disables callback work and releases user data after any in-flight callback. Metrics
  shutdown/destroy also removes the SDK's global provider registration when still current.
  Observer tokens cannot be handed to another thread, including work spawned by a callback.
  Repeatedly creating and destroying observables retains one disabled upstream registration
  per instrument until provider shutdown; applications should keep observable handles
  long-lived rather than creating them per request.
- The pinned SDK defaults to 2000 data points per instrument stream before aggregating
  overflow. Applications that intentionally require a different bound can configure a view
  cardinality limit.
- Public C recording attributes are scalar string/bool/int64/double values. Array attributes
  can be observed through the custom-export visitor when they originate in upstream resource
  or scope data, but there is no C recording API for array-valued measurement attributes.
- The supported shared-global deployment model is one shared API library on Linux or macOS,
  loaded before the matching SDK and retained for process lifetime after use. Windows
  shared-library use and static deployment are unsupported.
- Arbitrary third-party reader vtables are not exposed. The pinned upstream experimental
  reader trait is enabled only to implement the supported worker-free manual reader.
- Logs and asynchronous user-controlled collection are not exposed.
