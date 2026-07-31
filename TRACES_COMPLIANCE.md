# Experimental Traces compliance

This ledger records the implemented OpenTelemetry Traces surface for the current
experimental `0.x` C ABI, tracking the completion of the Traces workstream in
[issue #4](https://github.com/otap-labs/opentelemetry-c/issues/4). It mirrors
`METRICS_COMPLIANCE.md` and `LOGS_COMPLIANCE.md`.

Traces remain **experimental / Alpha**: the surface and ABI may change incompatibly between
`0.x` releases. Feature completeness alone does not create a stable ABI promise.

## Feature ledger

| Area | Status | Notes |
| --- | --- | --- |
| API-only operation | Implemented | Independent API-owned global `TracerProvider` slot; safe no-op tracers/spans before an SDK is installed, allocation-minimal, no global lookup on hot paths. |
| Tracer/provider/span handles | Implemented | Opaque handles with a common validated raw prefix; providers/tracers are `Send + Sync`, spans are single-thread. |
| Scalar span attributes | Implemented | String, bool, `int64`, and `double` attributes plus a tagged `otel_key_value_t` setter. |
| Span events | Implemented | Timestamped events with optional scalar attributes. |
| Span status | Implemented | `Unset`/`Ok`/`Error`; invalid codes rejected with `INVALID_ARGUMENT`. |
| Span name update | Implemented | `otel_span_update_name`. |
| Span kinds | Implemented | All stable kinds (`Internal`/`Server`/`Client`/`Producer`/`Consumer`); unknown values fall back to `Internal`. |
| Live local parent | Implemented | `otel_tracer_start_span` with an optional live parent span handle; a foreign-implementation parent is treated as a root span. |
| Context parent | Implemented | `otel_tracer_start_span_with_context` starts a span from an immutable, implementation-neutral `otel_span_context_t`. Mutually exclusive with a live parent handle. |
| Immutable SpanContext snapshot | Implemented | `otel_span_get_context` copies a live SDK-backed span's context into an API-owned handle; `clone`/`destroy` supported. Shared in-process by Traces and Logs. |
| SpanContext value operations | Implemented | Validity, 16-byte trace ID, 8-byte span ID, opaque `uint8_t` trace flags, `is_remote`, borrowed tracestate view, and construction from raw parts. Reserved/unknown trace-flag bits are preserved opaque. See "SpanContext value API" below. |
| W3C Trace Context propagation | Planned | Bounded direct `traceparent`/`tracestate` extract and inject; remote=true preserved; malformed input rejected. Baggage deferred (see below). |
| Span links | Planned | Links carrying an immutable `SpanContext` plus copied scalar attributes, via versioned span-start options. |
| Explicit start timestamp | Planned | Explicit Unix-nanosecond span start where the pinned Rust `SpanBuilder` permits. |
| Versioned span-start options | Planned | `struct_size`-gated span-start descriptor carrying kind, initial attributes, links, and start timestamp; older callers preserved. |
| Array-valued span attributes | Deferred | See "Deliberate limitations". |
| Built-in samplers | Planned | `AlwaysOn`, `AlwaysOff`, `TraceIdRatioBased`, `ParentBased` with a configurable root sampler. Custom sampler callbacks deferred. |
| Span limits | Planned | Max attributes/events/links per span and per-event/per-link attributes; spec defaults; overflow rejected. |
| Batch span processor | Implemented | Dedicated OS worker thread, spec-schedule export; SDK core. |
| Simple span processor | Planned | Synchronous export on the ending thread; suitable for tests and low-volume diagnostics. |
| OTLP Traces export | Implemented (HTTP) | HTTP/protobuf by default via the optional `otlp-http` feature. gRPC/tonic parity under evaluation (Phase 9). |
| Custom C trace exporter | Planned | Callback-based exporter with a callback-scoped read-only exported-span batch view; exactly-once user-data destruction. |
| Force flush / shutdown | Implemented | Deterministic force flush and shutdown through the SDK provider lifecycle. |
| Split-artifact linking | Implemented | Instrumentation links only the API library; the SDK is configured/linked separately; cross-artifact C tests assert exported semantics. |
| C and C++ headers | Implemented | Trace headers compile standalone as C11 and within the C++17 pipeline. |
| Hot path | Implemented | SDK-backed handles own concrete Rust objects; span operations dispatch through the per-handle vtable with no global lookup, lock, or pipeline allocation. |
| ABI compatibility | Implemented | Append-only trace vtable with `abi_version` + `struct_size` prefix checks; each optional capability gated on the offset of its final required field; frozen size boundaries asserted at compile time. |
| Status/error policy | Implemented | Signal-independent status classification and thread-local last-error diagnostics. |
| Resource bounds | Implemented (partial) | SDK builders bound span-processor and resource-attribute counts. Link/attribute-per-span bounds land with Phases 4 and 6. |

## SpanContext value API

`otel_span_context_t` is an immutable, API-owned, in-process snapshot. It does not expose any
Rust type or the SDK's internal `SpanContext` layout.

- **Sizes / byte representation.** The trace ID is exactly 16 bytes and the span ID exactly
  8 bytes, both in W3C big-endian (network) order — the same byte order used by the
  `traceparent` hex encoding. Trace flags are a single opaque `uint8_t`.
- **Validity.** A context is valid iff its trace ID is not all-zero **and** its span ID is not
  all-zero, matching the specification's `IsValid`.
- **Trace flags.** All 8 bits are preserved verbatim. Only the `sampled` bit (`0x01`) has
  defined meaning today; unknown/reserved bits are retained and never clear a context's
  validity. Masking happens only when writing into a narrower consumer representation that
  explicitly supports fewer bits (e.g. the Logs record view's supported-mask).
- **`is_remote`.** Preserved as supplied; contexts extracted from propagation headers report
  `is_remote == true`.
- **tracestate.** Returned as a borrowed UTF-8 view that is valid only for the duration of the
  accessor call (or, for the callback form, only until the visitor returns). Callers that need
  to retain it must copy the bytes. An empty tracestate is a zero-length view.
- **Construction from raw parts.** `otel_span_context_create` builds an owned context from a
  16-byte trace ID, 8-byte span ID, `uint8_t` flags, `is_remote`, and an optional tracestate
  view. Invalid (all-zero) IDs are rejected. tracestate is validated as UTF-8 and copied.
- **Ownership.** Every constructor/clone returns an owned handle released once with
  `otel_span_context_destroy` (NULL accepted).
- **Thread safety.** Snapshots are immutable and `Send + Sync`; they remain valid after the
  source span ends or is destroyed and may be read concurrently from multiple threads.
- **Allocation failure.** Construction/clone use fallible allocation and return NULL / a failure
  status with the last-error set rather than aborting.

## Lifecycle and threading guarantees

- Provider and tracer handles are safe to share and use concurrently. A single span handle must
  not be used concurrently from multiple threads; distinct spans are independent.
- No `*_destroy` may race with any other call on the same handle.
- `otel_span_end` is idempotent; destroying an un-ended span performs a best-effort end first.
- No Rust panic or unwind crosses an `extern "C"` boundary: every entry point is wrapped in a
  panic guard.

## ABI compatibility rules

- The public API library never depends on `opentelemetry_sdk`, OTLP, Tokio, HTTP, tonic, or
  exporter crates. Instrumentation links only `libopentelemetry_c_api`.
- The internal trace vtable is extended additively; existing fields are never reordered. Each
  appended capability has a named frozen size boundary (`OTEL_IMPL_VTABLE_*_SIZE`) pinned to
  literal 32- and 64-bit values with compile-time offset assertions.
- Capability presence is gated on the offset of the capability's final required field, never on
  `size_of` of the whole current vtable, so a newer API safely consumes an older SDK.
- `struct_size` never advertises more readable bytes than the local object contains; a full
  typed reference is never formed before a readable version/kind/size prefix is validated.

## Deliberate limitations and upstream constraints

- **Baggage is deferred.** W3C Trace Context (`traceparent`/`tracestate`) is in scope for this
  epic; Baggage is a separate cross-signal context concern and is intentionally not part of this
  PR. It can be added later as an additive module without destabilizing Trace Context.
- **No ambient/current-span context.** The C API does not add a global or thread-local
  "current span"; parenting is always explicit via a live span handle or a `SpanContext`.
- **Array-valued span attributes are deferred.** Public C span recording exposes scalar
  string/bool/int64/double attributes. Array attributes may be observable through export
  visitation when they originate upstream, but there is no C recording API for array-valued span
  attributes; introducing one cleanly requires a cross-signal attribute design tracked as a
  focused follow-up rather than a one-off type.
- **Custom sampler callbacks are deferred.** Only built-in samplers are exposed; an arbitrary C
  sampler callback would require lifetime/threading/reentrancy/panic contracts as strong as the
  custom exporter's and is out of scope for this PR.
- **Explicit end timestamp.** Only exposed if the pinned stable Rust API accepts one at span end;
  otherwise the SDK records the end time. Documented per the final implementation.

## Maturity status

Alpha / experimental. Not promoted beyond Alpha: the repository defines no stable-ABI maturity
criteria that this surface claims to satisfy.

## Issue #4 checklist status

Trace context and propagation:
- [x] Stable C representation/opaque handle for SpanContext (IDs, flags, remote, validity, tracestate).
- [x] Start spans from an extracted/explicit context (`otel_tracer_start_span_with_context`).
- [ ] W3C Trace Context inject/extract (Phase 3).
- [x] Baggage decision recorded: deferred to a separate cross-signal epic.
- [x] First propagation API avoids long-lived borrowed C memory / unconstrained callbacks (bounded direct API).

Span creation and data:
- [ ] Span links with attributes (Phase 4).
- [x] Array-valued attributes: deferred with rationale + follow-up (above).
- [ ] Explicit start timestamp and other stable `SpanBuilder` fields (Phase 4).
- [ ] Ended-span/no-op consistency for every new operation (validated per phase).

SDK configuration:
- [ ] Built-in samplers (Phase 5).
- [ ] Span limits (Phase 6).
- [x] ID-generator/user-callback decision: custom sampler/ID callbacks deferred; built-in only.
- [ ] Simple span processor (Phase 7).
- [ ] Custom/user-provided exporter with callback ownership + shutdown semantics (Phase 8).

Validation and usability:
- [ ] API-only no-op tests for all new calls.
- [ ] SDK-backed semantic tests (sampling, limits, links, context parenting, propagation).
- [ ] Extended C header compile tests and runnable C example.
- [ ] Cross-artifact tests proving the API-only caller uses the installed SDK.
- [ ] Hot-path benchmarks where new per-span operations are introduced.
- [ ] Documented supported/deferred behavior and ABI evolution rules (this file, updated per phase).
