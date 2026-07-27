# Custom C Logs exporter

Receives finished log batches in plain C, with no OTLP transport and no networking.

```sh
make run
```

## What it shows

* Registering a callback table with `otel_custom_log_exporter_new` and transferring the
  resulting exporter into a simple log processor.
* Walking the exported batch view: resource attributes, instrumentation scope, presence bits,
  body, attributes, and the flattened value-node pool used for arrays and maps.
* The callback-state lifecycle: `shutdown` runs once, then `state_destroy` runs exactly once
  after the last export callback has returned.

## Rules the example follows

* **Nothing escapes the callback.** Every pointer reachable from
  `otel_log_export_batch_view_t` dies when `export_logs` returns, so the example prints during
  the callback instead of storing views.
* **Presence bits before fields.** `timestamp_unix_nanos` and `trace_context` are only read
  when their `OTEL_LOG_EXPORT_FIELD_*` bit is set.
* **Children live at greater indices.** Container values address a contiguous range of
  `value_nodes` at strictly greater indices, which is what makes the recursive printer
  terminate without a visited set.
* **No reentrant shutdown.** The callback never calls back into the SDK, provider, processor,
  or exporter that is invoking it.

A batch log processor works the same way, except the callback runs on the processor's worker
thread rather than the emitting thread.
