# Experimental Metrics compliance

This ledger records the implemented OpenTelemetry Metrics surface for the current
experimental `0.x` C ABI.

| Area | Status | Notes |
| --- | --- | --- |
| API-only operation | Implemented | Independent API-owned global `MeterProvider`; no SDK dependency and safe no-op instruments before installation. |
| Synchronous instruments | Implemented | `u64`/`f64` counters, `i64`/`f64` up-down counters, `u64`/`i64`/`f64` gauges, and `u64`/`f64` histograms. |
| Observable instruments | Implemented | Counters, up-down counters, and gauges for all Rust SDK-supported numeric types; callback user data has exactly-once destruction. |
| Observer lifetime | Implemented | Observer tokens are valid only during their callback and reject stale use after return. Destroying the public instrument disables future callback work. |
| Instrument validation | Implemented | Name, unit, UTF-8, options structure size, and explicit histogram boundary validation occurs before SDK dispatch. |
| SDK pipeline | Implemented | Independent `SdkMeterProvider`, one or more periodic readers, resource/scope propagation, force flush, and shutdown. |
| OTLP Metrics | Implemented | HTTP/protobuf endpoint, headers, timeout, and cumulative/delta/low-memory temporality preference. |
| Views | Implemented | Exact or single-wildcard name selection, meter/unit/kind selection, stream metadata, attribute allow-list, cardinality limit, default/drop/sum/last-value/explicit histogram/base-2 exponential histogram aggregation. |
| Split-artifact linking | Implemented | C integration links separate API/SDK shared libraries and verifies OTLP Metrics bytes through the API-owned global slot. |
| C and C++ headers | Implemented | All Metrics headers compile standalone as C11; the combined pipeline headers compile as C++17. |
| Hot path | Implemented | SDK-backed synchronous handles own concrete Rust instruments; recording does not resolve providers, lock global state, or access readers/exporters/views. |
| ABI compatibility | Implemented | Separate Metrics vtable with version/size checks and provider replacement race coverage. |

## Known experimental constraints

- Metrics are experimental and may change incompatibly between `0.x` releases.
- The Rust 0.32 periodic reader controls collection on its worker thread; its force flush may
  block, and its shutdown uses the upstream reader's fixed timeout behavior.
- Observable callbacks cannot be unregistered from the upstream Rust SDK. Destroying the C
  handle disables callback work, while retained callback state is released when the SDK
  pipeline releases its closure.
- The supported shared-global deployment model is one dynamically loaded API library on
  Linux or macOS. Windows dynamic linking remains unverified.
- Logs, custom readers/exporters, and asynchronous user-controlled collection are not
  exposed.
