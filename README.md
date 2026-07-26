# OpenTelemetry C

A **Rust-backed C implementation** of OpenTelemetry traces and Metrics, delivered as one
component split into separate **API** and **SDK** libraries (plus an internal ABI crate).
This split lets a C or C-compatible instrumentation library depend only on the API, while
the application owns the SDK.

> [!WARNING]
> **Experimental and not production-ready.** The public C API and ABI may change between
> `0.x` minor releases. Pin the complete release and do not mix API and SDK artifacts from
> different tags.

## Project status

OpenTelemetry implementations mature at different rates by signal and component. A single
repository-wide label would hide that this project has broad Metrics coverage, partial trace
coverage, and no logs implementation.

| Signal | C API | Native ABI | C SDK | OTLP exporter | Current scope |
| --- | --- | --- | --- | --- | --- |
| **Traces** | Alpha, partial | Alpha | Alpha, partial | Alpha: HTTP/protobuf | Spans, events, scalar attributes, status, batch processing, and lifecycle are implemented. Sampling configuration, propagation, links, limits, and other items remain in the [traces epic](https://github.com/otap-labs/opentelemetry-c/issues/4). |
| **Metrics** | Alpha | Alpha | Alpha | Alpha: HTTP/protobuf and optional gRPC | Synchronous and observable instruments, periodic readers, views, temporality, and lifecycle are implemented. See the [compliance ledger](METRICS_COMPLIANCE.md) and [Metrics epic](https://github.com/otap-labs/opentelemetry-c/issues/5) for constraints and remaining extensions. |
| **Logs** | Not implemented | Not implemented | Not implemented | Not implemented | Tracked by the [logs epic](https://github.com/otap-labs/opentelemetry-c/issues/6). |

Component stability is also explicit:

| Component | Status | Compatibility meaning |
| --- | --- | --- |
| Public C API (headers and behavior) | Alpha | Experimental for the implemented trace and Metrics surfaces. |
| Public native ABI (`otel_*` symbols, layouts, ownership) | Alpha | No stable ABI promise before the project explicitly declares one. |
| C SDK and exporters | Alpha | Experimental pipeline configuration, lifecycle, and transport surface. |
| Internal API-to-SDK ABI crate and vtables | Internal | Version-checked for fail-closed dispatch, but not a public or third-party extension interface. |

These labels describe the maturity of **this C surface**, not the stability of the underlying
OpenTelemetry Rust crates or the OpenTelemetry specification. “Implemented” in a compliance
ledger describes feature coverage; it does not upgrade that component from Alpha.

Releases are **source-only**: one version and tag cover the API, SDK, and internal ABI
packages. No prebuilt native binaries or crates.io packages are distributed, and API/SDK
artifacts from different tags must not be mixed. Linux and macOS shared-library use are
supported; Windows shared-library use and static deployment are unsupported.

Build from source using [docs/BUILDING.md](docs/BUILDING.md). See
[VERSIONING.md](VERSIONING.md), [RELEASING.md](RELEASING.md),
[SECURITY.md](SECURITY.md), [CONTRIBUTING.md](CONTRIBUTING.md), and the
[examples](sdk/examples).

## Project structure

- **[api/](api/)** — package `opentelemetry-c-api`. The public C **API** facade (trace and
  Metrics providers/instruments as opaque handles). Owns independent process-global trace
  and Metrics provider slots with safe no-op defaults. Depends only on the internal ABI
  crate — never on the SDK/OTLP.
  Instrumentation links **this library only**.
- **[sdk/](sdk/)** — package `opentelemetry-c-sdk`. The **SDK**: OTLP HTTP/protobuf trace,
  HTTP/protobuf and optional gRPC Metrics exporters, a batch span processor, periodic
  Metrics readers, declarative Metrics views, and signal-specific lifecycle operations.
  Applications link **this plus the API**.
- **[abi/](abi/)** — package `opentelemetry-c-abi`. An **internal, Rust-only** rlib holding
  the shared `#[repr(C)]` types and the implementation vtable used across the API/SDK
  boundary. It has no exported C symbols and is not consumed directly by C.

## Consumption model

- **Instrumentation libraries** link only `libopentelemetry_c_api` (include `api.h`). Trace
  and Metrics calls are safe no-ops until an application installs the corresponding SDK
  provider, then they dispatch to it.
- **Applications** link `libopentelemetry_c_api` **and** `libopentelemetry_c_sdk` (include
  `sdk.h` plus the pipeline headers); they build the required signal pipelines, install
  their providers globally, flush, and shut down each configured signal.

## Getting started

Build the coordinated API and SDK libraries from one source release:

```sh
cargo build --locked --release -p opentelemetry-c-api -p opentelemetry-c-sdk
```

Then start with a complete, buildable example:

- [C traces](sdk/examples/c-basic-traces) — API instrumentation, OTLP exporter, batch
  processor, global installation, flush, and shutdown.
- [C Metrics](sdk/examples/c-metrics) — synchronous and observable instruments, periodic
  reader, views, OTLP export, and lifecycle.

See [Building from a source release](docs/BUILDING.md) for Cargo features, both required
header include roots, native linking, and platform constraints.

## Documentation

- [C API](api/README.md) — headers, ownership, thread safety, no-op behavior, and API-only
  instrumentation.
- [C SDK](sdk/README.md) — trace and Metrics pipelines, Cargo features, exporters, readers,
  views, and lifecycle.
- [Metrics compliance](METRICS_COMPLIANCE.md) — implemented surface and experimental
  constraints.
- [Performance](docs/PERFORMANCE.md) — hot-path contract and opt-in benchmarks.
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
