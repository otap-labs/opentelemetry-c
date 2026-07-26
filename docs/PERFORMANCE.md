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
cargo bench -p opentelemetry-c-sdk --bench metrics_allocations
```

- `api_hotpath` measures the API-only, no-SDK path. It isolates opaque-handle,
  panic-guard, and no-op dispatch costs. Its Metrics matrix records counter, gauge, and
  histogram operations with 0, 1, 4, 8, and 16 preconstructed integer/bool, mixed-numeric,
  and string attributes.
- `sdk_hotpath` installs a real exporter and SDK pipeline through the public C API and
  measures the C boundary plus Rust SDK processing. It repeats the same attribute matrix and
  includes direct OpenTelemetry Rust calls with equivalent preconstructed attributes, so C
  conversion/FFI overhead can be separated from SDK aggregation cost. Its benchmark target
  requires the `otlp` feature.

Both suites separate pipeline setup, global installation, and tracer or meter acquisition
from measured loops. Attribute keys, string values, C arrays, and Rust `KeyValue` arrays are
also built outside the timed loops. The API-only path remains no-op before SDK installation,
so attributed calls do not convert or allocate SDK values. The SDK benchmark sends to a
closed loopback port and uses a one-hour collection interval, so it is not an exporter or
network-throughput benchmark.

Criterion records time per operation under stable groups:
`api_no_sdk_metrics_attributes`, `sdk_backed_metrics_attributes`, and
`rust_sdk_metrics_attributes`. Published results should include `rustc -Vv`, the target,
release profile, and Cargo feature flags.

`metrics_allocations` reports steady-state allocation count and allocated bytes per operation
for the same Metrics matrix. It warms each case before enabling a process-local counting
allocator and uses a custom exporter plus manual reader for the C SDK path, so no worker,
collection, export, or network activity can contaminate the measurements. The counters cover
allocations made by all threads while a sample is active; this benchmark intentionally creates
no background worker threads.

Run the repeatable Linux VM protocol with:

```sh
METRICS_BENCH_REPEATS=3 scripts/benchmark-metrics.sh
```

The script records the exact SHA, Rust and Cargo versions, kernel, CPU, memory, persistent-disk
space, load, profile, features, command output, and GNU `time -v` peak RSS. Criterion reports
the median estimate, confidence interval, and outliers. Compare repeated runs only when VM load
is materially similar. These results are informational; shared VM timing is not a CI gate, and
regression thresholds require a history from stable hardware.

Both suites call the real `#[no_mangle] extern "C"` symbols used by C consumers. Header
compilation and the C examples cover C source-level linkage separately. Any future
exporter/network benchmark should remain opt-in and outside the default regression set.

## Sanitizer validation

Linux sanitizer runs are explicit because they require nightly Rust, `rust-src`, Clang, and
substantially more time and disk than ordinary CI:

```sh
rustup toolchain install nightly --component rust-src
scripts/sanitize-metrics.sh address
scripts/sanitize-metrics.sh thread
scripts/sanitize-metrics.sh leak
scripts/sanitize-metrics.sh undefined
```

Address, thread, and leak modes instrument the Rust standard library, API and SDK tests, and
the custom-exporter/manual-reader split-library C flow. UndefinedBehaviorSanitizer instruments
the C harness while exercising the normal Rust shared libraries because rustc does not expose
a general UBSan mode. Cross-artifact tests honor `CARGO_BUILD_TARGET`, `CARGO_TARGET_DIR`, and
simple whitespace-separated `CFLAGS` so instrumented target-triple builds are found and the C
executable links the corresponding sanitizer runtime.
