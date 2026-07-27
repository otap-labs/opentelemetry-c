# Changelog

## Unreleased

### Added

- Experimental Logs pipeline: an independent `SdkLoggerProvider` with its own global slot and
  lifecycle (`otel_sdk_set_logs_as_global`, `otel_sdk_logs_force_flush`,
  `otel_sdk_logs_shutdown`), simple and batch log processors, and OTLP Logs export over
  HTTP/protobuf and optional gRPC. Log record conversion validates the caller's flat value
  node pool completely before converting any of it, uses `try_reserve` for the
  caller-sized bulk allocations (the collections whose capacity a caller controls), and sets
  trace context explicitly so the ambient Rust context can never leak into a C caller's
  record. Individual string copies use ordinary infallible allocation, as elsewhere in the
  crate. `otel_sdk_logs_force_flush` takes no timeout and there is no batch export-timeout
  setter, because the pinned upstream APIs support neither; see `LOGS_COMPLIANCE.md`.

### Fixed

- SDK builders now bound transferred processors, Metrics readers, views, resource attributes,
  and per-view attribute lists before allocation or worker growth can become unbounded.

### Added

- Metrics hot-path benchmarks now expose attribute-count and value-type scaling for counter,
  gauge, and histogram recording, with direct OpenTelemetry Rust baselines for separating C
  conversion/FFI overhead from SDK aggregation cost.
- Optional async periodic Metrics reader behind `metrics-async-runtime`. It owns one bounded
  Tokio runtime, maps the upstream cooperative export timeout, supports multiple readers,
  requires no caller-managed runtime, and shuts its runtime down after reader/provider
  destruction. The timeout cannot preempt synchronous custom callback execution. Custom
  exporters are supported; the blocking OTLP/HTTP and synchronous OTLP/gRPC wrappers are
  rejected.
- C callback-backed Metrics exporters and worker-free manual readers. Export callbacks
  traverse callback-scoped resource, scope, metric, point, histogram, exponential histogram,
  exemplar, and scalar/array attribute views; stale/cross-thread batch use fails closed.
  Manual collection is driven synchronously through `otel_sdk_metrics_force_flush`, with
  callback failure propagation and exactly-once shutdown/state destruction.
- SDK opaque handles now use the coordinated raw handle prefix and globally unique kinds,
  matching API-side validation before complete typed access.
- Complete instrumentation scopes now propagate attributes to OTLP, and Metrics views can
  select exact scope version, schema URL, and required typed scope attributes.
- Documented the coordinated, experimental, source-only product release policy. Consumers
  build matching API and SDK libraries from one tag; no native binaries or crates.io
  packages are distributed.
- Optional OTLP/gRPC Metrics export with explicit additive C transport and compression
  selectors. HTTP/protobuf remains the default. The gRPC path owns one bounded Tokio runtime
  per exporter, maps existing string headers to validated ASCII tonic metadata, supports
  transport-specific gzip/zstd features and opt-in tonic TLS, and requires no Rust runtime
  management by C applications. HTTP-only builds exclude tonic and all gRPC exporter
  dependencies; gRPC-only builds exclude reqwest. Reqwest's pre-existing blocking client
  continues to resolve its own transitive Tokio dependency.
- Experimental Metrics SDK: OTLP HTTP/protobuf exporter, periodic reader, multiple-reader
  pipelines, independent Metrics global install/flush/shutdown, cumulative/delta/low-memory
  temporality, declarative views, attribute allow-lists, cardinality limits, explicit and
  base-2 exponential histograms, synchronous/observable C examples, and Metrics hot-path
  benchmarks.
- Cross-artifact C integration now proves API-only Metrics export through separately linked
  API and SDK shared libraries and decodes the OTLP payload to verify resource, scope,
  synchronous and observable values, attributes, and histogram data. C11/C++17 header
  compilation covers all Metrics headers.
- Metrics global installation now uses conditional registration tokens so shutdown/destroy
  releases the current global provider without clearing a newer SDK. Backing SDK callback
  state is released exactly once across success, validation failure, and caught construction
  panics. Periodic-reader destruction actively shuts down its worker and exporter, and
  explicit Metrics shutdown followed by SDK destruction does not shut the pipeline down twice.
- Exporter/processor separation of concerns with **optional OTLP**. The generic
  `otel_trace_exporter_t` / `otel_span_processor_t` handles now wrap internal enums
  (`TraceExporterImpl: SpanExporter`, `SpanProcessorImpl: SpanProcessor`), and the SDK builder
  stores a homogeneous `Vec<SpanProcessorImpl>` — so the SDK core is coupled to neither OTLP
  nor the batch processor, and a new exporter/processor kind is a variant plus a builder (no C
  ABI change). `opentelemetry-otlp` and `reqwest` are now **optional** behind the default-on
  `otlp` feature; `--no-default-features` builds the SDK core without them or any TLS backend.
  The `otel_otlp_trace_exporter_builder_*` symbols remain in every configuration;
  `otel_otlp_trace_exporter_builder_build` returns `OTEL_STATUS_INVALID_CONFIG` when `otlp` is
  disabled. Public C ABI, headers, and default-feature behavior are unchanged. For reqwest 0.13
  the TLS features are `native-tls` (default) and `rustls-tls` (→ `reqwest/rustls`).

- Initial release of `opentelemetry-c-sdk` as part of the split of `opentelemetry-c` into
  separate C **API** and **SDK** artifacts. The SDK library provides the OTLP HTTP/protobuf
  exporter, batch span processor, and `otel_sdk_*` lifecycle behind the C ABI. Installing as
  global (or fetching a provider handle) registers the SDK's implementation into the API
  cdylib's global provider slot across the C ABI, so API-only instrumentation observes it.

  Selectable TLS backend (`native-tls` default, or `rustls-tls`); bounded C-provided batch
  sizes; panic-safe entry points; local parent/child span semantics; force-flush and
  shutdown. A `cross_artifact` integration test proves API-only spans export through the SDK
  across the separate cdylibs.
- Pipeline object model: the SDK setup is decomposed into a generic trace exporter
  (`otel_trace_exporter_t`) and span processor (`otel_span_processor_t`), built by the OTLP
  trace exporter builder (`otlp_trace_exporter.h`) and batch span processor builder
  (`batch_span_processor.h`), and assembled through `otel_sdk_builder_add_span_processor`.
  OTLP/batch-specific setters were removed from the SDK builder. Builders transfer ownership
  of their children on `OTEL_STATUS_OK`. The generic exporter/processor handles are opaque
  extension points for future exporter/processor kinds without an ABI break.
- Criterion benchmark `sdk_hotpath` measuring the SDK-backed hot path (tracer acquisition
  through the installed global provider, span start/end, attribute setters, and a bounded
  event) with a real OTLP-exporter + batch-processor pipeline. It runs with no collector
  required (the exporter targets a closed loopback port, so background export attempts may fail
  fast and are discarded), is not an export/throughput benchmark, and is not a CI gate. Run
  explicitly with `cargo bench -p opentelemetry-c-sdk`. See `opentelemetry-c/README.md` for
  details.
- `otel_otlp_trace_exporter_builder_add_header` now rejects a duplicate header key
  (case-insensitively, so `Authorization` and `authorization` collide) with
  `OTEL_STATUS_INVALID_ARGUMENT` (and a `otel_last_error_message()` diagnostic) instead of
  silently overwriting the previously added value.

### Changed

- Ownership-transfer documentation now states the actual uniform contract: a successful
  transfer consumes the handle and immediately invalidates its original pointer. Tests no
  longer access or destroy consumed pointers.
- Documented and tested the existing distinction between export-pipeline failures and
  wrapper/infrastructure failures under the shared status policy.
