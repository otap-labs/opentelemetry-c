<!-- SPDX-License-Identifier: Apache-2.0 -->

# Custom C Traces exporter

Receives finished span batches in plain C, with no OTLP transport and no networking.

```sh
make run
```

## What it shows

* Registering a callback table with `otel_custom_trace_exporter_new` and transferring the
  resulting exporter into a simple span processor.
* Walking the materialized span batch view: resource attributes, per-span name, kind, status,
  trace/span IDs, attributes (scalar and one-level array tags), events, and links.
* Emitting spans through the SDK's own tracer provider, so the whole pipeline — including the
  callback state — is released on `otel_sdk_destroy`.
* The callback-state lifecycle: `shutdown` runs once, then `state_destroy` runs exactly once
  after the last export callback has returned.

## Rules the example follows

* **Nothing escapes the callback.** Every pointer reachable from
  `otel_span_export_batch_view_t` dies when `export_spans` returns, so the example prints
  during the callback instead of storing views.
* **Attribute tags before payload.** Array tags (`OTEL_SPAN_ATTRIBUTE_TYPE_*_ARRAY`) select
  the `array` union member; every other tag selects `scalar`. Span attributes never nest, so
  there is no recursive value pool as there is for logs.
* **No reentrant shutdown.** The callback never calls back into the SDK, provider, processor,
  or exporter that is invoking it, and never unwinds across the C ABI boundary.

A batch span processor works the same way, except the callback runs on the processor's worker
thread rather than the emitting thread.
