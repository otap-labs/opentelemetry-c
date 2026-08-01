# OpenTelemetry C

A **Rust-backed C implementation** of OpenTelemetry traces, Metrics, and Logs, delivered as one
component split into separate **API** and **SDK** libraries (plus an internal ABI crate).
This split lets a C or C-compatible instrumentation library depend only on the API, while
the application owns the SDK.

> [!WARNING]
> **Experimental and not production-ready.** The public C API and ABI may change between
> `0.x` minor releases. Pin the complete release and do not mix API and SDK artifacts from
> different tags.

## Project status

All three signals are available, but the C API and ABI remain experimental.

| Signal | C API | Native ABI | C SDK | OTLP exporter | Current scope |
| --- | --- | --- | --- | --- | --- |
| **Traces** | Alpha | Alpha | Alpha | HTTP/protobuf and optional gRPC | Spans, context propagation, sampling, processors, OTLP export, and custom C export. See [TRACES_COMPLIANCE.md](TRACES_COMPLIANCE.md). |
| **Metrics** | Alpha | Alpha | Alpha | HTTP/protobuf and optional gRPC | Synchronous and observable instruments, readers, views, OTLP export, and custom C export. See [METRICS_COMPLIANCE.md](METRICS_COMPLIANCE.md). |
| **Logs** | Experimental | Experimental | Experimental | HTTP/protobuf and optional gRPC | Structured log bridge, trace correlation, processors, OTLP export, and custom C export. See [LOGS_COMPLIANCE.md](LOGS_COMPLIANCE.md). |

“Implemented” describes feature coverage, not API or ABI stability. See
[VERSIONING.md](VERSIONING.md) for the compatibility policy.

Releases are **source-only**: one version and tag cover the API, SDK, and internal ABI
packages. No prebuilt native binaries or crates.io packages are distributed, and API/SDK
artifacts from different tags must not be mixed. Linux and macOS shared-library use are
supported; Windows shared-library use and static deployment are unsupported.

Build and install from source using [docs/BUILDING.md](docs/BUILDING.md) and
[docs/PACKAGING.md](docs/PACKAGING.md). See
[VERSIONING.md](VERSIONING.md), [RELEASING.md](RELEASING.md),
[SECURITY.md](SECURITY.md), [CONTRIBUTING.md](CONTRIBUTING.md), and the
[examples](sdk/examples).

## Project structure

- **[api/](api/)** — package `opentelemetry-c-api`. The public C **API** facade (trace and
  Metrics providers/instruments and Logs providers/loggers as opaque handles). Owns
  independent process-global trace, Metrics, and Logs provider slots with safe no-op
  defaults. Depends only on the internal ABI
  crate — never on the SDK/OTLP.
  Instrumentation links **this library only**.
- **[sdk/](sdk/)** — package `opentelemetry-c-sdk`. The **SDK**: optional OTLP
  HTTP/protobuf and gRPC exporters for Traces, Metrics, and Logs; span processors,
  periodic Metrics readers, declarative Metrics views, simple and batch log processors, and
  signal-specific lifecycle operations.
  Applications link **this plus the API**.
- **[abi/](abi/)** — package `opentelemetry-c-abi`. An **internal, Rust-only** rlib holding
  the shared `#[repr(C)]` types and the implementation vtable used across the API/SDK
  boundary. It has no exported C symbols and is not consumed directly by C.

## Consumption model

- **Instrumentation libraries** link only `libopentelemetry_c_api` (include `api.h`). Trace,
  Metrics, and Logs calls are safe no-ops until an application installs the corresponding SDK
  provider, then they dispatch to it. Each signal has its own independent global slot.
- **Applications** link `libopentelemetry_c_api` **and** `libopentelemetry_c_sdk` (include
  `sdk.h` plus the pipeline headers); they build the required signal pipelines, install
  their providers globally, flush, and shut down each configured signal.

## Getting started

Build and install the coordinated API and SDK libraries from one source release:

```sh
cmake -S . -B build \
  -DCMAKE_BUILD_TYPE=Release \
  -DCMAKE_INSTALL_PREFIX="$PWD/install"
cmake --build build --parallel
cmake --install build
```

Installed consumers can use `find_package(OpenTelemetryC CONFIG REQUIRED)` or the
`opentelemetry-c-api` and `opentelemetry-c-sdk` pkg-config modules. Direct Cargo builds,
local Conan and vcpkg recipes, and the Homebrew formula generator are documented in the
[Packaging Guide](docs/PACKAGING.md).

Then start with a complete, buildable example:

- [C traces](sdk/examples/c-basic-traces) — API instrumentation, OTLP exporter, batch
  processor, global installation, flush, and shutdown.
- [C metrics basic](sdk/examples/c-metrics-basic) — smallest complete Metrics lifecycle:
  SDK + meter + counter + manual collection + shutdown.
- [C metrics instruments](sdk/examples/c-metrics-instruments) — instrument-kind-focused usage:
  counter, up/down counter, gauge, histogram, bound, and observable instruments.
- [C manual metrics](sdk/examples/c-manual-metrics) — deterministic application-controlled
  collection with a manual reader.
- [C custom metric exporter](sdk/examples/c-custom-metric-exporter) — callback-based exporter,
  batch visitor traversal, and exporter lifecycle.
- [C periodic metrics](sdk/examples/c-periodic-metrics) — periodic reader interval-driven
  collection/export and graceful shutdown.
- [C Metrics (all-in-one)](sdk/examples/c-metrics) — combined OTLP Metrics reference.
- [C Logs](sdk/examples/c-logs) — experimental log bridge: logger acquisition, structured
  record values, trace correlation, batch processor, OTLP export, and lifecycle.
- [C custom Logs exporter](sdk/examples/c-custom-log-exporter) — receive finished log batches
  in your own C code, with no OTLP transport: callback registration, batch-view traversal, and
  callback-state ownership.
- [C custom trace exporter](sdk/examples/c-custom-trace-exporter) — receive finished span
  batches in your own C code, with no OTLP transport: callback registration, span-batch-view
  traversal (attributes, events, links), and callback-state ownership.

See [Building from a source release](docs/BUILDING.md) for feature selection and platform
constraints.

## Documentation

- [C API](api/README.md) — headers, ownership, thread safety, no-op behavior, and API-only
  instrumentation.
- [C SDK](sdk/README.md) — trace, Metrics, and Logs pipelines, Cargo features, exporters,
  readers, views, processors, and lifecycle.
- [Traces compliance](TRACES_COMPLIANCE.md) — implemented surface and experimental
  constraints.
- [Metrics compliance](METRICS_COMPLIANCE.md) — implemented surface and experimental
  constraints.
- [Logs compliance](LOGS_COMPLIANCE.md) — implemented surface and the deliberate
  `event_name`/`target` omissions.
- [Performance](docs/PERFORMANCE.md) — hot-path contract and opt-in benchmarks.
- [Packaging](docs/PACKAGING.md) — CMake installation and consumer integration.
- [Versioning](VERSIONING.md) and [releasing](RELEASING.md) — compatibility and source-only
  release policy.
- [Contributing](CONTRIBUTING.md) and [security](SECURITY.md).

## Supported model

The supported model is Linux or macOS shared-library use with exactly one API library loaded
before the matching SDK library. Keep both loaded after use; `dlclose` is unsupported.
Windows shared-library use and static deployment are unsupported. See
[VERSIONING.md](VERSIONING.md#supported-shared-library-model).

## License

Apache-2.0.
