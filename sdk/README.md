# opentelemetry-c-sdk

[![Apache License][license-image]][license-url]

The **C SDK** of the Rust-backed OpenTelemetry C binding: OTLP **HTTP/protobuf** trace
export, HTTP/protobuf and optional gRPC Metrics export, a batch span processor, periodic
Metrics readers, and declarative Metrics views behind C functions. Installing a signal
provider registers it into the **API library's** corresponding global slot, so
instrumentation that links only [`opentelemetry-c-api`](../api) exports through it.

HTTP uses the blocking `reqwest` client. The optional Metrics gRPC transport owns one
bounded Tokio runtime per exporter and keeps it alive through reader/provider shutdown.
In either case, **no user-managed async runtime is required**.

> ⚠️ **Experimental.** The C ABI is not yet stable and may change between `0.x` releases.

## Linking model

Applications link **both** libraries and put both include directories on the search path
(this header includes the API's `common.h`/`trace.h`). Instrumentation libraries link only
the API. The SDK cdylib references the API cdylib's internal registration symbols, resolved
at load time — so the application must link the API alongside the SDK. This load-time
resolution is verified on **Unix-like dynamic linking (Linux, macOS)**; **Windows is not yet
verified** and needs an import-library follow-up (see the API README's Platform support).

```sh
cargo build --release -p opentelemetry-c-api -p opentelemetry-c-sdk

cc -std=c11 my_app.c \
   -I path/to/opentelemetry-c/api/include \
   -I path/to/opentelemetry-c/sdk/include \
   -L path/to/target/release -lopentelemetry_c_api -lopentelemetry_c_sdk \
   -Wl,-rpath,path/to/target/release -o my_app
```

Static linking may require additional native/system libraries from the Rust, TLS, and HTTP
dependencies; discover them with
`cargo rustc --release --lib -- --print native-static-libs`.

### Library lifetime

The shared-global model requires **dynamic linking with exactly one loaded
`libopentelemetry_c_api`** (see the API README).

Once `otel_sdk_set_as_global` succeeds, it publishes this crate's `'static` implementation
vtable and an SDK-owned provider object into the API's global slot. **`otel_sdk_shutdown`
and `otel_sdk_destroy` do not clear that slot** — they stop and free the `otel_sdk_t`
handle, but the slot keeps referencing this library's vtable/provider. The slot is cleared
only when **another provider replaces it** (a subsequent `otel_sdk_set_as_global` /
registration).

Therefore, after a successful global install, **`libopentelemetry_c_sdk` must remain loaded
until process exit, or until another provider replaces the global slot** — shutting down and
destroying the SDK does **not** make unloading it safe. Any live SDK-backed handles (tracer
provider, tracer, span obtained after `set_as_global`) must also be destroyed before unload.
Statically linking the API into multiple artifacts creates separate global slots and is
**not** the shared-global model.

Metrics global installation is intentionally different: each successful
`otel_sdk_set_metrics_as_global` receives an internal registration token.
`otel_sdk_metrics_shutdown` and `otel_sdk_destroy` remove the Metrics global reference only
when that token still owns the slot. If another SDK installed a newer Metrics provider, the
older SDK's shutdown is a no-op for the global slot. Explicitly acquired MeterProvider/meter
handles remain caller-owned and must be destroyed before unloading the SDK library.

Ready-to-run examples that link both libraries are in
[`examples/c-basic-traces/`](examples/c-basic-traces) and
[`examples/c-metrics/`](examples/c-metrics).

For minimal API-only instrumentation and application SDK setup snippets, see the
[root component README](../README.md#minimal-c-usage).

## Pipeline object model

The SDK builds a trace pipeline from separate, composable objects that map to OpenTelemetry
concepts, so the SDK builder is not coupled to any one exporter or processor:

```
OTLP exporter builder ──build──▶ otel_trace_exporter_t
                                        │ set_exporter (ownership transfers)
                                        ▼
batch span processor builder ─build─▶ otel_span_processor_t
                                        │ add_span_processor (ownership transfers)
                                        ▼
                 SDK builder ──build──▶ otel_sdk_t ──set_as_global──▶ global provider
```

For traces, only the **OTLP HTTP/protobuf exporter** and the **batch span processor** are
implemented today. The generic `otel_trace_exporter_t` / `otel_span_processor_t` handles are
opaque extension points: internally each wraps an enum (`TraceExporterImpl` implementing
`SpanExporter`, `SpanProcessorImpl` implementing `SpanProcessor`), so another exporter or
processor kind is a new variant plus a builder — no change to the C ABI, the generic handles,
or the SDK builder's storage. No custom-callback exporter is provided yet.

Metrics uses a parallel pipeline:

```
OTLP Metrics exporter builder ──build──▶ otel_metric_exporter_t
                                                │ set_exporter
                                                ▼
periodic reader builder ─────────build──▶ otel_periodic_metric_reader_t
                                                │ add_metric_reader
declarative view builder ────────build──▶ otel_metric_view_t
                                                │ add_metric_view
                           SDK builder ──build──▶ otel_sdk_t
```

Multiple readers and views may be added before build. Metrics installation, force flush, and
shutdown are independent from trace lifecycle.

### Cargo features (optional OTLP)

The **SDK core** is separate from any exporter implementation. HTTP/protobuf remains the
default and the existing `otlp` feature remains a compatibility alias:

| Feature | Default | Effect |
| --- | --- | --- |
| `otlp` | ✅ (via `native-tls`) | Compatibility alias for `otlp-http`. |
| `otlp-http` | ✅ | OTLP HTTP/protobuf traces and Metrics using blocking reqwest. |
| `otlp-grpc` | ❌ | OTLP/gRPC Metrics using tonic and an SDK-owned Tokio runtime. |
| `native-tls` | ✅ | Implies `otlp-http`; HTTP HTTPS via the platform TLS stack. |
| `rustls-tls` | ❌ | Implies `otlp-http`; HTTP HTTPS via rustls. |
| `grpc-tls-ring` | ❌ | Implies `otlp-grpc`; tonic TLS using the ring provider and native/platform roots. |
| `otlp-http-gzip`, `otlp-http-zstd` | ❌ | HTTP compression for the selected algorithm. |
| `otlp-grpc-gzip`, `otlp-grpc-zstd` | ❌ | gRPC compression for the selected algorithm. |

Building with `--no-default-features` produces the SDK core without any OTLP transport. All
OTLP builder symbols remain present; requesting a transport that was not compiled returns
`OTEL_STATUS_INVALID_CONFIG` with a useful last-error message. HTTP-only builds contain no
tonic or gRPC exporter dependencies, and gRPC-only builds contain no reqwest dependency.
Reqwest 0.13 itself still resolves Tokio transitively even for its blocking client; issue #13
does not add or use an SDK-owned Tokio runtime on the HTTP path.

Metrics transport is selected only by
`otel_otlp_metric_exporter_builder_set_transport`; endpoint syntax never changes transport.
HTTP endpoints normally include `/v1/metrics`, while gRPC endpoints normally contain only
scheme and authority, such as `http://localhost:4317`. Programmatic endpoints override the
upstream OTLP environment endpoint.

The existing header setter maps to HTTP headers or validated ASCII gRPC metadata. Duplicate
keys are rejected case-insensitively. Binary `-bin` metadata is unsupported, and diagnostics
name an invalid key without exposing its value. Compression must be compiled for the selected
transport or build fails; it is never silently disabled. Plaintext `http://` gRPC works with
`otlp-grpc`; `https://` additionally requires `grpc-tls-ring`. Custom certificates and keys
are not exposed.

### Ownership transfer rules

- A `build(builder, &out)` call creates a new owned object; the builder stays owned by the
  caller (destroy it when done).
- `otel_batch_span_processor_builder_set_exporter` transfers the exporter into the processor
  builder **on `OTEL_STATUS_OK`**; on failure the caller still owns it.
- `otel_sdk_builder_add_span_processor` transfers the processor into the SDK builder **on
  `OTEL_STATUS_OK`**; on failure the caller still owns it.
- After a successful transfer, do **not** destroy the transferred handle (its destroy becomes
  a safe no-op).
- Destroying a builder frees any transferred children it still owns (i.e. that a later
  `build` did not consume). All `*_destroy` functions are NULL-safe and must not race with
  other use of the same handle.

## Headers

- [`include/opentelemetry_c/sdk.h`](include/opentelemetry_c/sdk.h) — SDK builder, resource
  config, `add_span_processor`, build, and lifecycle (`set_as_global`, `get_tracer_provider`,
  `force_flush`, `shutdown`, `destroy`).
- [`otlp_trace_exporter.h`](include/opentelemetry_c/otlp_trace_exporter.h) — OTLP HTTP/protobuf
  exporter builder (endpoint / header / timeout).
- [`batch_span_processor.h`](include/opentelemetry_c/batch_span_processor.h) — batch processor
  builder (exporter + bounded queue/delay/batch settings).
- [`trace_exporter.h`](include/opentelemetry_c/trace_exporter.h) /
  [`span_processor.h`](include/opentelemetry_c/span_processor.h) — the generic opaque handles.
- [`otlp_metric_exporter.h`](include/opentelemetry_c/otlp_metric_exporter.h) — OTLP Metrics
  transport, endpoint, headers/metadata, compression, timeout, and temporality preference.
- [`periodic_metric_reader.h`](include/opentelemetry_c/periodic_metric_reader.h) — periodic
  export interval and exporter ownership. Reader shutdown timeout behavior is controlled by
  the pinned upstream SDK.
- [`metric_view.h`](include/opentelemetry_c/metric_view.h) — instrument selection, stream
  metadata, attribute filtering, cardinality, and aggregation.

## Behavior & guarantees

- Application owns the pipeline + SDK lifecycle: build exporter → build processor → build SDK
  → `set_as_global` → (instrumentation emits spans via the API) → `force_flush` → `shutdown`.
  Shutdown runs at most once.
- Batch queue / export-batch sizes from C are bounded; oversized values are rejected with
  `OTEL_STATUS_INVALID_ARGUMENT`, `0` selects the SDK default.
- All entry points are panic-safe. Runtime export failures never crash the process.
- gRPC runtime creation, channel construction, and transport selection happen only during
  exporter construction/export. Synchronous Metrics recording performs no runtime lookup,
  transport branch, exporter access, allocation, or additional lock.
- The SDK library never re-exports the API/trace/common functions, so linking both
  libraries produces no duplicate symbols.

## Tests

`cargo test -p opentelemetry-c-sdk --all-features` covers trace and Metrics vtable behavior,
global registration, callback lifetime, batch bounds, and force-flush cleanup. The
`cross_artifact` integration test compiles a C program, links it against **both** built
cdylibs, and confirms API-only spans and Metrics export through the SDK. With `otlp-grpc`,
it also runs a bounded local tonic MetricsService and proves that an ordinary C process with
no Tokio runtime exports, flushes, shuts down, and destroys the pipeline.

Because `cargo test` does not emit cdylib artifacts, build them first:

```sh
cargo build -p opentelemetry-c-api -p opentelemetry-c-sdk --all-features
cargo test -p opentelemetry-c-sdk --test cross_artifact --all-features
```

The repository's `scripts/test.sh` performs this build step automatically. The test
self-skips only as a local developer convenience (missing C compiler or unbuilt cdylibs);
under `CI` it fails hard instead, so the proof can never silently no-op. Verified on
Unix-like dynamic linking (Linux, macOS); see the API README for the Windows status.

## License

Apache-2.0.

[license-image]: https://img.shields.io/badge/license-Apache_2.0-green.svg
[license-url]: https://github.com/open-telemetry/opentelemetry-rust-contrib/blob/main/LICENSE
