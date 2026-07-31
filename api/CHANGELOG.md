# Changelog

## Unreleased

### Added

- Extended span start: `otel_tracer_start_span_ex` accepts a versioned
  `otel_span_start_options_ex_t` (a `struct_size`-first descriptor) carrying span links
  (`otel_span_link_t`: a borrowed `otel_span_context_t` plus optional link attributes), an
  explicit start timestamp (`start_time_unix_nanos`, 0 = unset), initial span attributes, and a
  single parenting source (`parent` or `parent_context`, mutually exclusive). Optional fields
  are read only when `struct_size` covers them, so older and newer headers interoperate; unknown
  tail fields are ignored. A backed implementation predating this entry fails closed with
  `OTEL_STATUS_INVALID_CONFIG`; an unbacked tracer returns a valid no-op span. See
  `TRACES_COMPLIANCE.md`.
- W3C Trace Context propagation: a bounded, direct API operating on the immutable
  `otel_span_context_t` (no SDK, vtable, or global state). `otel_trace_propagation_extract`
  parses a `traceparent` plus optional `tracestate` into a new owned **remote** context;
  `otel_trace_propagation_inject_traceparent` / `_inject_tracestate` format an existing context
  into caller-provided buffers with a length-query/undersized contract. Strict lowercase-hex,
  version/flag/separator/all-zero-ID validation; unknown trace-flag bits preserved; input sizes
  bounded before allocation. Baggage remains deferred. See `TRACES_COMPLIANCE.md`.
- SpanContext value operations over the immutable `otel_span_context_t`:
  `otel_span_context_is_valid`, `otel_span_context_is_remote`, `otel_span_context_trace_id`
  (16-byte big-endian), `otel_span_context_span_id` (8-byte big-endian),
  `otel_span_context_trace_flags` (opaque `uint8_t`, all bits preserved),
  `otel_span_context_tracestate` (borrowed UTF-8 view valid until the context is destroyed),
  and `otel_span_context_create` to build an owned context from raw parts. All-zero IDs are
  rejected; unknown/reserved trace-flag bits are kept opaque. See `TRACES_COMPLIANCE.md`.
- API-owned immutable `otel_span_context_t` snapshots. A context can be copied from a live
  SDK-backed span, cloned across threads, used as an implementation-neutral parent for a new
  span, and attached directly to a log record without manually copying trace/span IDs.
- Experimental Logs API: an API-owned global `LoggerProvider` slot independent of the trace
  and Metrics slots, API-only no-op loggers, versioned logger options carrying a complete
  instrumentation scope, severity-based `otel_logger_enabled`, and a borrowed one-shot
  `otel_log_record_view_t` whose structured values are expressed through a flat node pool.
  Pool nodes may reference children only at a strictly greater index, so cycles are
  structurally unrepresentable. `event_name` and `target` are deliberately not exposed; see
  `LOGS_COMPLIANCE.md`.

### Fixed

- Observable instrument creation failures now preserve caller ownership of callback user data
  consistently, including failures after the backing SDK accepts and releases callback state.

### Added

- API-only Metrics benchmarks now measure counter, gauge, and histogram recording with
  preconstructed integer/bool, mixed-numeric, and string attributes at 0, 1, 4, 8, and 16
  attributes.
- Common raw opaque-handle prefixes now validate project handle kind before complete typed
  access, allowing live wrong-type handles to fail closed while preserving caller lifetime
  obligations for foreign, freed, or concurrently destroyed pointers.
- Versioned meter options now expose complete instrumentation scopes, including copied typed
  scope attributes with consistent API-only validation and duplicate-key rejection.
- Documented the coordinated, experimental, source-only product release policy. The API,
  SDK, and ABI packages share one tag and are not published independently.

- Experimental Metrics API with independent API-owned global `MeterProvider`, API-only no-op
  behavior, all nine synchronous numeric instrument combinations, all seven observable
  combinations, typed observer callbacks, strict instrument validation, and callback-scoped
  observer lifetime enforcement.
- Versioned, signal-namespaced internal Metrics vtable with kind/version and size compatibility
  checks that reject trace vtables before dispatch, plus race-safe provider replacement.
- Prefix-only ABI validation rejects truncated vtables before forming a full Rust reference.
  Backing SDK callback-state ownership is released exactly once even when construction panics,
  and token-based Metrics deregistration releases the global provider on Metrics
  shutdown/destroy without clearing a newer installation.
- Initial release of `opentelemetry-c-api` as part of the split of `opentelemetry-c` into
  separate C **API** and **SDK** artifacts. The API library exposes the public trace API
  (tracer providers, tracers, spans) as opaque handles, owns the process-global trace provider
  slot with a no-op default, and exposes the internal registration ABI the SDK
  uses to install itself. It depends only on the internal ABI-types crate — never on
  `opentelemetry_sdk`, `opentelemetry-otlp`, or `reqwest` — so instrumentation can link the
  API alone. Existing FFI-safety hardening is preserved (fixed-width discriminants,
  best-effort handle contract, panic firewall, documented thread/lifecycle contracts).
- Criterion benchmark `api_hotpath` measuring the API-only, no-SDK (no-op provider) hot-path
  FFI boundary cost (global provider / tracer acquisition, span start/end, scalar and string
  attribute setters). Run explicitly with `cargo bench -p opentelemetry-c-api`; not a test or
  CI gate. See `opentelemetry-c/README.md` for details.
- Optional header-only convenience helpers over the raw C API (no new ABI symbols, no Rust
  changes): typed `otel_key_value_t` constructors `otel_kv_string` / `otel_kv_bool` /
  `otel_kv_int64` / `otel_kv_double` (`common.h`) and span-status shorthands
  `otel_span_set_ok` / `otel_span_set_error` (`trace.h`). They are `static inline` (guarded for
  C99+/C++ like the existing `otel_cstr`), build POD by value with no allocation/copy, and
  (for the status shorthands) perform exactly the one `otel_span_set_status()` call they wrap.

### Changed

- Trace and Metrics ABI kind/version/size incompatibilities now consistently report
  `OTEL_STATUS_INVALID_CONFIG`; the public status classification policy is documented.
- Observable dispatch now uses callback-thread-local registrations instead of a
  process-global mutex. Observer tokens fail closed on another thread or after callback
  return, while concurrent reader callbacks and same-thread reentrant observations do not
  serialize on API-global state.
