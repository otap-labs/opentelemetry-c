# Changelog

## Unreleased

### Added

- General API-owned context parenting through a new append-only trace-vtable capability. The
  SDK validates the versioned borrowed context view and converts its SpanContext into the
  upstream Rust `Context` only for span construction; existing explicit SpanContext and
  extended-start slots remain unchanged for mixed-version compatibility.

- OTLP gRPC/tonic transport for Traces plus
  `otel_otlp_trace_exporter_builder_set_transport` and
  `otel_otlp_trace_exporter_builder_set_compression`, bringing Traces exporter selection to
  parity with Metrics and Logs. The gRPC transport owns one single-worker Tokio runtime per
  exporter and is gated by `otlp-grpc` (with `otlp-grpc-gzip`/`otlp-grpc-zstd` for
  compression and `grpc-tls-ring` for TLS roots).
- C callback-backed Traces exporter (`otel_custom_trace_exporter_new`), usable with span
  processors without any OTLP feature. The export callback receives a callback-scoped,
  read-only span batch view with resource, scope, span, event, and link data, plus
  scalar/one-level-array attributes. Callback state transfers on `OTEL_STATUS_OK` only and
  `state_destroy` runs exactly once after in-flight exports complete. A runnable
  `c-custom-trace-exporter` example demonstrates callback registration, span-batch-view
  traversal, and callback-state lifecycle.
- Simple span processor: `otel_simple_span_processor_create` consumes a trace exporter and
  produces a generic `otel_span_processor_t` that exports each finished span synchronously on
  the thread that ended it. It takes ownership of the exporter on `OTEL_STATUS_OK` (the pointer
  becomes invalid) and leaves it caller-owned on failure, matching the batch builder's
  transfer contract. Intended for tests, short-lived programs, and debugging; production
  pipelines should prefer the batch span processor.
- Configurable trace span limits: `otel_sdk_builder_set_span_limits` caps the number of
  attributes, events, and links retained per span (and attributes per event/link) from a
  versioned `otel_span_limits_t` (`struct_size`-gated). Values map directly to the SDK's
  `SpanLimits`; a NULL config restores the spec defaults (128 for every bound); a non-zero
  reserved field or an undersized `struct_size` is rejected. Overflowing items are dropped by
  the SDK (most-recently-added first), matching the specification.
- Built-in trace sampler configuration: `otel_sdk_builder_set_sampler` selects the tracer
  provider's root sampler from a versioned `otel_sampler_config_t` (`struct_size`-gated).
  Supported kinds are `AlwaysOn`, `AlwaysOff`, `TraceIdRatioBased` (probability in `[0, 1]`),
  and `ParentBased` wrapping a configurable non-parent-based root sampler. Passing a NULL
  config restores the SDK default (`ParentBased(AlwaysOn)`); invalid ratios, reserved bytes,
  and a parent-based root that is itself parent-based are rejected. Custom sampler callbacks
  remain deferred; see `TRACES_COMPLIANCE.md`.
- Trace vtable support for the extended span-start entry (`tracer_start_span_ex`): the SDK
  reconstructs span contexts and links from a borrowed forward-only descriptor and forwards
  span links, an explicit start timestamp, initial attributes, and a single parenting source
  into the OTel `SpanBuilder`. The entry is appended after the SpanContext prefix and gated by a
  new frozen `OTEL_IMPL_VTABLE_SPAN_START_EX_SIZE` capability boundary.
- Trace vtable support for copying complete `SpanContext` snapshots (including trace state and
  remote state) and starting children from implementation-neutral snapshots used by the C API
  and Logs correlation path.
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

- C callback-backed Logs exporters (`otel_custom_log_exporter_new`), usable with either the
  simple or the batch log processor and requiring no OTLP transport feature. The export
  callback receives a callback-scoped, read-only batch view that reuses `otel_log_value_t` and
  the same flat node-pool invariants as the emit path, so one traversal routine serves both
  directions. Conversion is breadth-first, allocation-checked, and enforces the existing
  `OTEL_LOG_MAX_*` limits plus a record-count limit, failing the export rather than truncating
  silently; map entries are sorted by key because the pinned map type has no stable iteration
  order. Callback state transfers on `OTEL_STATUS_OK` only and `state_destroy` runs exactly
  once, after the last in-flight export callback returns. The callback table is versioned by a
  required prefix ending at `export_logs`, so tables compiled against older or newer releases
  are both accepted and members outside the caller's `struct_size` are never read. There is
  deliberately no force-flush callback, because the pinned `LogExporter` trait has no
  force-flush operation, and a failing export callback is only observable to the C caller under
  the batch processor; see `LOGS_COMPLIANCE.md`.

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
