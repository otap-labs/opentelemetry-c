# Performance contract and benchmarks

The C API/SDK is a thin ABI boundary over the Rust OpenTelemetry SDK. It must not add runtime
machinery on telemetry hot paths beyond required FFI marshalling and the Rust SDK's own
internals. This is a standing design invariant.

## Hot-path contract

Setup and cold paths may allocate and use locks. These include SDK, exporter, processor,
reader, view, and resource builders; OTLP and batch configuration; `otel_sdk_build`;
global-provider installation; force-flush and shutdown coordination; tests; and examples.

Span, tracer, and synchronous Metrics hot paths must not add, at the C layer:

- new locks, `OnceLock`s, registries, or global maps;
- C-side batching or intermediate telemetry records;
- per-operation clones of providers, exporters, processors, readers, views, or configuration;
- exporter, processor, reader, or view access;
- environment-variable or configuration lookups;
- callbacks into user code; or
- routing beyond the signal-specific API-to-SDK implementation vtable.

SDK-backed Metrics handles own the concrete Rust instrument, so synchronous recording
performs no provider lookup, global lock, registry lookup, exporter access, or callback
dispatch.

Accepted and required hot-path costs include:

- opaque-handle validation;
- API-to-SDK vtable dispatch, normally once per operation;
- validation of C pointers, tags, and lengths;
- conversion of borrowed C strings and attributes into SDK-owned values;
- allocation of the real OpenTelemetry objects and C handles; and
- the Rust SDK's own processing.

`otel_span_destroy` may call both `span_end` and `span_free` to preserve best-effort
end-before-free semantics. Converting borrowed C string, key, and value views currently
requires owned allocations because C memory must not outlive the call.

The global-provider `RwLock` read and `Arc` clone occur only when resolving a tracer from the
global provider with `otel_tracer_provider_get_tracer`, never per span, attribute, or event.
Cache and reuse the returned tracer. Span operations take no global lock at the C API/vtable
layer. Entry points that report failures clear the thread-local last-error slot at entry;
that clear takes no global lock and allocates no heap memory.

Observable Metrics callbacks are collection-path work rather than synchronous recording.
Observer dispatch uses callback-thread-local registrations rather than a process-global
mutex, so readers collecting concurrently do not serialize at the C API boundary. A
deterministic API test holds two observer dispatches inside the SDK-facing operation at once
to guard this property. Callback and observer-lifetime rules are documented in
[`metrics.h`](../api/include/opentelemetry_c/metrics.h) and
[`METRICS_COMPLIANCE.md`](../METRICS_COMPLIANCE.md).

## Benchmarks

Two [Criterion](https://crates.io/crates/criterion) suites measure trace and Metrics
recording. They run explicitly, are not part of `cargo test` or a required CI gate, and do
not require a collector:

```sh
cargo bench -p opentelemetry-c-api
cargo bench -p opentelemetry-c-sdk
```

- `api_hotpath` measures the API-only, no-SDK path. It isolates opaque-handle,
  panic-guard, and no-op dispatch costs.
- `sdk_hotpath` installs a real exporter and SDK pipeline through the public C API and
  measures the C boundary plus Rust SDK processing. Its benchmark target requires the
  `otlp` feature.

Both suites separate pipeline setup, global installation, and tracer or meter acquisition
from measured loops. The SDK benchmark sends to a closed loopback port, so it is not an
exporter or network-throughput benchmark.

Both suites call the real `#[no_mangle] extern "C"` symbols used by C consumers. Header
compilation and the C examples cover C source-level linkage separately. Any future
exporter/network benchmark should remain opt-in and outside the default regression set.
