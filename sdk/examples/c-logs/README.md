# C Logs example (experimental)

> [!WARNING]
> The Logs C API is **experimental** and may change in a future release.

This example builds an OTLP log exporter and a batch log processor, installs the SDK
LoggerProvider into the API-owned global Logs slot, emits records through the API-only
path, then flushes and shuts down:

```sh
make run
```

Set `OTEL_EXPORTER_OTLP_LOGS_ENDPOINT` to override `http://localhost:4318/v1/logs`.

## What to look at

**The split.** Everything after `otel_sdk_set_logs_as_global()` uses only `logs.h` from the
API library. That is exactly how an instrumentation library behaves: it links the API alone,
and its calls are safe no-ops until an application installs an SDK.

**The node pool.** Structured values (maps and arrays) are not a pointer graph. The record
carries one flat array of `otel_log_key_value_t` and refers to sub-values by index range. A
node may only reference children at a **strictly greater** index, so cycles cannot be
expressed and the SDK can validate an entire record without a visited set. See
`emit_structured()` for a two-level example.

**Borrowing.** Every pointer reachable from `otel_log_record_view_t` is borrowed for the
duration of `otel_logger_emit()` only. The SDK validates and copies everything it retains
before returning, so all of the example's records live on the stack.

**Ownership transfer.** `set_exporter` and `add_log_processor` consume their argument on
`OTEL_STATUS_OK` only. On failure the caller still owns it — which is why the error paths in
this example destroy the object they were about to transfer.

## Known limitations

- **`event_name` is not exposed.** The pinned upstream Rust setter takes a `&'static str`,
  which cannot be satisfied from borrowed C memory without leaking or interning. The record
  struct is `struct_size`-versioned, so it can be added without breaking layout.
- **`target` is not exposed.** Upstream uses it to *override* the instrumentation scope name,
  which would silently corrupt scope reporting for callers that already supply a scope.
- **No unsigned 64-bit values.** The pinned `AnyValue` has no `u64` variant; use `int64` or a
  string.
- **`otel_sdk_logs_force_flush()` ignores its timeout.** The pinned provider flush takes no
  timeout parameter and blocks until every processor finishes.
- **The batch export timeout is not applied.** The pinned Logs batch configuration exposes no
  export-timeout setter; use the `OTEL_BLRP_EXPORT_TIMEOUT` environment variable.

See [LOGS_COMPLIANCE.md](../../../LOGS_COMPLIANCE.md) for the full ledger.
