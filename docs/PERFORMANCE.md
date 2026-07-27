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

Experimental bound counter and histogram handles move C attribute validation, copying, and
SDK binding to the one-time bind call. Their steady-state C recording path performs opaque
handle validation, clears the thread-local error slot, makes one API-to-SDK vtable call, and
invokes the upstream bound `add` or `record`; it passes no attribute pointer, converts no
attribute, and introduces no C-layer allocation, lock, registry lookup, or reference-count
operation. This is the thinnest attributed Metrics path supported by OpenTelemetry Rust 0.32.

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
for the same Metrics matrix, including pre-bound counter and histogram recordings. It warms
each case before enabling a process-local counting
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

### Linux baseline

A three-repeat informational baseline completed at
`4efa9e5d4066ed80e186e9f57017ab7aec8c030d` on Linux 6.17, Rust/Cargo 1.95.0,
and a 48-vCPU Intel Xeon 6973P-C Azure VM with 188 GiB RAM. Initial load averages were
0.66/0.60/0.59. The benchmark used the release profile and default SDK features
(`otlp-http` via `native-tls`).

Representative counter results below show the three Criterion point estimates. The final
column is the confidence interval from the repeat whose point estimate is the median of the
three:

| Path and attributes | Repeat estimates | Median estimate | Representative interval |
| --- | ---: | ---: | ---: |
| API-only/no-SDK, 0 attributes | 3.938 / 3.940 / 3.942 ns | 3.940 ns | 3.933–3.947 ns |
| API-only/no-SDK, 16 integer/bool | 3.956 / 3.946 / 3.940 ns | 3.946 ns | 3.941–3.952 ns |
| C SDK, 0 attributes | 19.077 / 19.535 / 19.069 ns | 19.077 ns | 19.018–19.140 ns |
| C SDK, 1 integer/bool | 86.323 / 86.098 / 84.048 ns | 86.098 ns | 86.000–86.206 ns |
| C SDK, 16 integer/bool | 948.340 / 942.390 / 945.470 ns | 945.470 ns | 942.080–949.110 ns |
| C SDK, 1 string | 118.740 / 117.720 / 118.020 ns | 118.020 ns | 117.840–118.190 ns |
| C SDK, 16 strings | 1731.100 / 1728.000 / 1740.900 ns | 1731.100 ns | 1728.200–1733.700 ns |
| Direct Rust SDK, 0 attributes | 8.128 / 8.070 / 7.407 ns | 8.070 ns | 8.038–8.103 ns |
| Direct Rust SDK, 16 integer/bool | 241.030 / 242.290 / 240.610 ns | 241.030 ns | 240.850–241.200 ns |
| Direct Rust SDK, 16 strings | 316.960 / 321.040 / 318.340 ns | 318.340 ns | 318.060–318.650 ns |

The full counter/gauge/histogram matrix completed. Repeat variation was generally small, but
the shared VM produced noisy sample sets: for example, the second zero-attribute C SDK counter
run classified 36/100 samples as outliers while its point estimate remained within 2.4% of
the other repeats. One direct-Rust single-attribute case varied more substantially. These
results therefore establish scale and a reproducible baseline, not regression thresholds.

Allocation results were identical across all three repeats:

| Recording path | 0 attributes | 1 integer/bool | 16 integer/bool | 1 string | 16 strings |
| --- | ---: | ---: | ---: | ---: | ---: |
| API-only/no-SDK | 0 | 0 | 0 | 0 | 0 |
| Direct Rust SDK after warmup | 0 | 0 | 0 | 0 | 0 |
| C SDK allocations/op | 0 | 2 | 17 | 3 | 33 |
| C SDK allocated bytes/op | 0 | 245 | 1238 | 263 | 1526 |

Warm-run peak RSS was 82.8–87.1 MiB for `api_hotpath`, 333.4–393.9 MiB for
`sdk_hotpath`, and 73.2–73.7 MiB for `metrics_allocations`. First-run RSS was higher because
Cargo compilation occurred inside the timed command and is not treated as steady-state
benchmark memory.

Both suites call the real `#[no_mangle] extern "C"` symbols used by C consumers. Header
compilation and the C examples cover C source-level linkage separately. Any future
exporter/network benchmark should remain opt-in and outside the default regression set.

### Logs (experimental)

```sh
cargo bench -p opentelemetry-c-sdk --bench logs_hotpath
cargo bench -p opentelemetry-c-sdk --bench logs_allocations
LOGS_BENCH_REPEATS=3 scripts/benchmark-logs.sh
```

Logs are benchmarked differently from Metrics because the cost is dominated by *conversion*
rather than aggregation. A borrowed `otel_log_record_view_t` may not outlive its call, so every
string, byte string, map, and array it references has to be copied into owned Rust storage on
every emit. Both suites therefore sweep four body shapes — absent, a plain string, a
64-byte byte string, and a two-level map containing an array — against 0, 1, 3, 5, and 6
attributes. The counts 5 and 6 are not arbitrary: the pinned `SdkLogRecord` keeps capacity for
five attributes inline, so 6 is the first count that spills to the heap, and reporting only
round numbers would hide that step. Two cases sit outside the matrix and are measured on their
own: a trace-correlated record, because correlation is the one field the bridge must set
explicitly on every emit to stop the ambient Rust `Context` from leaking into a C caller's
record, and `otel_logger_enabled` on a severity that is answered false, because that is the only
Logs call a well-written C application makes when a level is disabled — it is the cost of *not*
logging.

Each shape is measured on three paths so the numbers can be attributed rather than merely
observed: `noop` (public C API, no SDK installed), `c_sdk` (public C API over a real SDK Logs
pipeline), and `rust` (the pinned Rust SDK driven directly with equivalent data). The gap
between `noop` and `c_sdk` is the bridge; the gap between `c_sdk` and `rust` is the honest price
of the C boundary.

`logs_allocations` is the more useful of the two for regression detection, because allocation
counts are deterministic while timings on a shared machine are not. It configures a small
bounded batch queue and a one-hour scheduled delay, and warms until the queue is full, so every
measured emit performs the complete validate-convert-handoff and is then dropped by the queue.
That is deliberate: an unbounded queue would fold amortized queue reallocation into the
per-record figures, and allowing an export to run would let background-thread allocations be
charged to whichever emit happened to be in flight, since the counting allocator is global.

The result that matters most is that every `noop` case reports **exactly zero allocations and
zero bytes per operation**. A C application that links the API but installs no SDK pays no heap
traffic for logging at all, which is the property a C caller most needs to be able to depend on.

Indicative figures from a developer machine (Apple silicon, macOS, release profile, default
features) — informational only, not a baseline, and not comparable to the Linux VM numbers
above:

| Shape / attributes | `noop` allocs/op | `c_sdk` allocs/op | `rust` allocs/op |
| --- | --- | --- | --- |
| no body, 0 | 0 | 2.3 | 1.4 |
| no body, 3 | 0 | 15.5 | 11.2 |
| no body, 5 | 0 | 24.3 | 18.8 |
| no body, 6 | 0 | 29.3 | 23.4 |
| string body, 0 | 0 | 6.5 | 2.1 |
| string body, 6 | 0 | 32.4 | 24.4 |
| bytes body, 0 | 0 | 9.0 | 6.3 |
| bytes body, 6 | 0 | 34.0 | 30.0 |
| nested body, 0 | 0 | 30.4 | 12.5 |
| nested body, 6 | 0 | 57.3 | 35.7 |
| trace-correlated, 1 | 0 | 11.4 | — |

Reading these honestly: scalar attributes cost the bridge roughly one extra allocation each
over direct Rust, which is the unavoidable copy out of caller memory. Byte-string bodies track
the Rust baseline closely because both must own the buffer. Nested bodies cost proportionally
more because the node pool is converted bottom-up into owned `AnyValue` containers, and that
gap — not the scalar path — is where any future optimization should be aimed. Trace correlation
costs roughly half an allocation over the same record without it. The `noop` column shows the C
boundary itself contributes nothing when logging is disabled.

One combination in the specification is deliberately absent: a C-driven simple processor with an
in-memory exporter. The in-memory exporter is a test-only construct in the SDK crate and is not
reachable as a C handle, and substituting a real OTLP exporter behind a simple processor would
measure a connection-refused syscall per record rather than the bridge. The simple processor is
covered by correctness tests instead; the batch processor is what a production C caller uses and
is what is measured here.

The corresponding `logs_hotpath` timings on the same machine put a no-SDK emit at roughly 3.4 ns
regardless of payload — the API rejects the work before inspecting it — against roughly 106 ns
for the simplest SDK-backed emit and roughly 900 ns for a nested body with six attributes, with
the direct Rust baseline about 20–30% below the C figures across the matrix.

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

Set `METRICS_SANITIZER_STRESS_ITERATIONS` to repeat the provider race, in-flight exporter
shutdown, multiple async reader, and concurrent install/shutdown tests inside address, thread,
or leak mode:

```sh
METRICS_SANITIZER_STRESS_ITERATIONS=10 scripts/sanitize-metrics.sh address
```

`scripts/sanitize-logs.sh` takes the same four modes and the same
`LOGS_SANITIZER_STRESS_ITERATIONS` knob. It exists separately because the Logs bridge stresses
sanitizers differently from Metrics: one `otel_logger_emit` call borrows a record, an attribute
array, and an arbitrarily nested pool of caller-owned value nodes, none of which may be read
past or retained after the call returns. Address and leak modes are therefore the real evidence
that the two-pass validator/converter respects those borrows, and the mode runs
`logs_cross_artifact` because only the two-cdylib layout has a genuine C owner for the buffers.
Thread mode covers emission racing shutdown and the separate global LoggerProvider slot.

## Lifecycle stress

Deterministic lifecycle tests use barriers, channels, and condition variables to force the
relevant ordering. Repeat the highest-risk Metrics cases without retries or sleeps:

```sh
METRICS_STRESS_ITERATIONS=100 scripts/stress-metrics.sh
```

The loop covers provider replacement and retention, concurrent installs, older-SDK shutdown,
destroy-without-shutdown, observable callback versus destruction, exporter export versus
shutdown, multiple manual and async readers, and fail-closed async reentrancy. A failed
iteration stops immediately; rerunning a failure is not treated as a pass.

The Logs equivalent is:

```sh
LOGS_STRESS_ITERATIONS=100 scripts/stress-logs.sh
```

It covers the global LoggerProvider slot under concurrent readers and re-registrations,
emission racing shutdown, installation racing shutdown, one-shot shutdown, implicit slot
clearing on drop, the independence of the Logs and Metrics global slots, and batch queue
saturation crossed with repeated pipeline creation and destruction. These cases are
worth repeating rather than running once because a lost race normally still produces a passing
interleaving; a single green run is close to no evidence at all.
