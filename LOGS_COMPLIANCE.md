# Experimental Logs compliance

This ledger records the implemented OpenTelemetry Logs surface for the current experimental
`0.x` C ABI. The Logs bridge is newer and narrower than Traces and Metrics: it is a
**log bridge**, intended for a logging library to route records through OpenTelemetry, not a
general-purpose end-user logging API.

| Area | Status | Notes |
| --- | --- | --- |
| API-only operation | Implemented | Independent API-owned global `LoggerProvider`, separate from the Trace and Metrics slots; no SDK dependency and safe no-op loggers before installation. |
| Logger acquisition | Implemented | Name-only and versioned options carrying name, version, schema URL, and uniquely keyed typed scope attributes into the upstream owned `InstrumentationScope`. |
| Record emission | Implemented | One-shot borrowed `otel_log_record_view_t`; the SDK validates and copies everything it retains before `otel_logger_emit()` returns. No pointer into caller memory escapes the call. |
| Severity | Implemented | Numbers 1–24 with `0` meaning absent. Out-of-range values are rejected with `OTEL_STATUS_INVALID_ARGUMENT` and emit nothing. |
| Severity text | Implemented (synthesized) | Derived from the normalized number via the upstream `Severity::name()` (`"TRACE"`…`"FATAL4"`). Caller-supplied text is not expressible; see constraints. |
| Body | Implemented | Any value kind, or absent. `OTEL_LOG_VALUE_TYPE_EMPTY` is accepted **only** as an absent body, because the pinned `AnyValue` has no null variant. |
| Attributes | Implemented | Up to 256 top-level attributes of any value kind. Duplicate keys are preserved in order, matching the pinned record, which appends without deduplication. |
| Structured values | Implemented | Strings, bools, `int64`, doubles, raw bytes, arrays, and maps, expressed through a flat borrowed node pool addressed by index range. |
| Node pool safety | Implemented | A node may reference children only at a strictly greater index, so cycles are structurally unrepresentable and validation needs no visited set. Every node must be referenced exactly once. |
| Limits | Implemented | 256 attributes, 1024 value nodes, depth 16, 1 MiB strings, 1 MiB byte payloads, 256 map entries, 256 array elements — all enforced before any conversion work. |
| Trace correlation | Implemented | Explicit trace id, span id, and the `SAMPLED` flag. Trace context is always set before the upstream emit, so the ambient Rust `Context` can never leak into a C caller's record. |
| Timestamps | Implemented | Optional `timestamp`; an omitted `observed_timestamp` is defaulted by the upstream SDK rather than by this bridge. |
| Level check | Implemented | `otel_logger_enabled()` maps to the upstream `event_enabled`. Severity `0` and values above 24 return false without entering Rust, since the upstream signature takes a non-optional `Severity`. |
| SDK pipeline | Implemented | Independent `SdkLoggerProvider`, simple and batch log processors, resource/scope propagation, force flush, and one-shot shutdown. |
| Custom exporter | Implemented | C callback-backed `otel_log_exporter_t` created by `otel_custom_log_exporter_new`, usable with either log processor. The export callback receives a callback-scoped, read-only batch view that reuses `otel_log_value_t` and the same flat node-pool invariants as the emit path. Callback state transfers on `OTEL_STATUS_OK` only and is released exactly once, after the last in-flight export returns. |
| OTLP Logs | Implemented | HTTP/protobuf by default plus optional gRPC/tonic, explicit transport selection, endpoint, headers/ASCII metadata, timeout, and transport-specific compression. |
| Lifecycle independence | Implemented | The Logs global slot, lock, and shutdown flag are separate from Trace and Metrics; shutting down one signal never disturbs another. Shutdown unregisters the global slot *before* stopping the provider, and only ever clears this SDK's own registration token. |
| Split-artifact linking | Implemented | A C integration test links separate API/SDK shared libraries and decodes the exported OTLP protobuf to verify records reached the SDK through the API-owned global slot. |
| C and C++ headers | Implemented | All Logs headers compile standalone as C11 under `-Wall -Wextra -Werror`. |
| Status/error policy | Implemented | Shares the signal-independent classification used by Traces and Metrics, with last-error diagnostics. |
| Resource bounds | Implemented | SDK builders accept at most 64 log processors. Capacity is reserved before ownership transfer, so a rejected transfer always leaves the object caller-owned. |
| `event_name` | **Not implemented** | See constraints. |
| `target` | **Not implemented** | See constraints. |
| Unsigned 64-bit values | **Not implemented** | The pinned `AnyValue` has no `u64` variant. |
| Logs SDK limits (attribute count/length truncation) | Not implemented | The pinned `SdkLoggerProvider` exposes no configurable log record limits; this bridge enforces its own ABI bounds instead. |

## Known experimental constraints

- Logs are experimental and may change incompatibly between `0.x` releases.

- **The custom exporter has no force-flush callback.** The pinned `LogExporter` trait has no
  force-flush operation, so the SDK would never invoke one. Provider force-flush is handled
  entirely by the log processor, which then exports through the ordinary export callback.

- **The custom exporter's export callback is read-only and callback-scoped.** Every pointer
  reachable from `otel_log_export_batch_view_t` dies when the callback returns. Nothing may be
  retained; a bridge must copy what it needs before returning.

- **A custom export callback must not reenter the SDK it is exporting for.** Both pinned
  processors export inside a telemetry-suppressed scope, so a log record emitted from the
  callback is dropped rather than recursing, but shutting down or destroying the SDK,
  provider, processor, or exporter from inside the callback self-deadlocks: the simple
  processor holds its exporter mutex and the exporter holds its own shutdown read lock across
  the call.

- **Conversion failures for a custom exporter are all-or-nothing.** A record that cannot be
  represented within the ABI limits (an oversized value, an unrepresentable value kind, or a
  pre-epoch timestamp) fails the whole export rather than being silently truncated or
  substituted. With a batch processor the accompanying last-error diagnostic is recorded on
  the processor's worker thread, so it is not visible to the C caller.

- **Exported map keys are reproduced verbatim.** Unlike the emit path, which rejects empty and
  duplicate map keys, the export path never rewrites legal upstream data. Map entries are
  sorted by key so exports are deterministic, since the pinned map type is a `HashMap`.

- **`event_name` is not exposed.** The pinned `LogRecord::set_event_name` takes a
  `&'static str`. Satisfying that from borrowed C memory would require either leaking every
  distinct event name for the process lifetime or maintaining an unbounded intern table — both
  are unbounded memory growth driven by caller input, and neither is acceptable in a bridge.
  No substitute field is written, and no attribute is silently invented in its place; the
  cross-artifact test asserts the field is **absent** on the wire. `otel_log_record_view_t` is
  `struct_size`-versioned and append-only, so `event_name` can be added without breaking
  layout once upstream accepts an owned string.

- **`target` is not exposed.** `set_target` does accept an owned `String`, so it is
  technically expressible. It is deliberately omitted because the pinned OTLP transform uses
  `target` to **override** `instrumentation_scope.name`. Exposing it would let one field
  silently rewrite the scope a caller already supplied through `otel_logger_options_t`,
  producing telemetry that disagrees with the caller's own configuration.

- **Severity text cannot be caller-supplied.** `set_severity_text` also takes a
  `&'static str`, so the text is synthesized from the number. This is lossless for
  spec-conformant severities but means a caller cannot carry through a source-specific label
  such as `"WARNING"` or `"crit"`. Put it in an attribute if it matters.

- **No unsigned 64-bit values.** The pinned `AnyValue` is
  `Int(i64) | Double(f64) | String | Boolean | Bytes | ListAny | Map`. Values above
  `i64::MAX` must be sent as a string, and doing so is the caller's explicit choice rather
  than a silent lossy cast.

- **Duplicate keys behave differently by position.** Duplicate **map** keys are rejected,
  because the pinned map type is a `HashMap` that would silently drop one entry. Duplicate
  **top-level attribute** keys are accepted and preserved, because the pinned record appends
  without deduplication, and enforcing uniqueness would cost a per-emit hash set on the hot
  path. Both behaviors are asserted by tests.

- **`otel_sdk_logs_force_flush()` takes no timeout**, unlike the Trace and Metrics
  equivalents. The pinned `SdkLoggerProvider::force_flush()` accepts none. Its synchronous
  batch processor nevertheless applies an internal, non-configurable five-second wait per
  flush request; expiry is surfaced by the provider as a non-OK export-pipeline result while
  the worker may still be exporting. Carrying a caller timeout for cross-signal symmetry was
  rejected because this wrapper could not honor it. If upstream adds configurable support, it
  can be exposed through a new function such as `otel_sdk_logs_force_flush_with_timeout()`
  without changing this function's C signature.

- **There is no batch export-timeout setter.** The pinned Logs `BatchConfigBuilder` exposes
  no `with_max_export_timeout` (unlike the traces one). Rather than accept a value and return
  `OTEL_STATUS_OK` for configuration that is never applied, the entry point is omitted; it
  can be added compatibly once upstream supports it. The synchronous batch processor applies
  no separate per-export deadline and does not read `OTEL_BLRP_EXPORT_TIMEOUT`; configure the
  OTLP exporter's transport timeout with
  `otel_otlp_log_exporter_builder_set_timeout_millis()` instead.

  Both of the above are the same judgement: while this surface is experimental it is cheaper
  to omit a knob than to freeze a no-op one that callers may come to depend on.

- **Emission returns no delivery guarantee.** `OTEL_STATUS_OK` from `otel_logger_emit()`
  means the record was validated, converted, and handed to the Rust logger. The upstream
  `Logger::emit` returns `()`, so downstream processor or exporter failure is not observable
  at the call site; it surfaces at the provider lifecycle boundary.

- **The simple log processor exports on the emitting thread**, inside `otel_logger_emit()`.
  It is intended for tests and low-volume diagnostics. Use the batch processor in production.

- **OTLP gRPC owns a bounded Tokio runtime** per exporter, as with Metrics. C callers do not
  supply a runtime. Exporting from inside an already-entered Tokio runtime is rejected with an
  export error rather than panicking in `block_on`.

- **gRPC binary `-bin` metadata is not exposed**, matching the Metrics exporter: arbitrary C
  strings are not reinterpreted as binary metadata values.

- **No logs-specific SDK limits.** The pinned SDK has no configurable log record attribute
  count or value length limits, so this bridge enforces only its own ABI bounds. A record that
  exceeds them is rejected outright rather than silently truncated, so a caller never
  discovers loss only by reading the backend.
