# c-periodic-metrics

Periodic collection/export with the public periodic reader and a callback exporter.

## What this example teaches

- Build a periodic reader with `otel_periodic_metric_reader_builder_set_interval_millis`.
- Transfer a custom exporter into that reader.
- Record measurements across multiple collection intervals.
- Handle background callback execution safely with atomics.
- Force flush and shut down cleanly.

## Prerequisites

- Built `opentelemetry-c-api` and `opentelemetry-c-sdk` libraries.
- A C11 compiler.

## Build

```sh
make
```

## Run

```sh
make run
```

## Expected output

A line similar to:

```text
periodic reader exported 2 batch(es)
```

## Ownership and lifetime notes

- `otel_periodic_metric_reader_builder_set_exporter` consumes exporter ownership on success.
- `otel_sdk_builder_add_metric_reader` consumes reader ownership on success.
- Visitor pointers from `otel_metric_batch_visit` are borrowed and callback-scoped.

## Threading notes

- Periodic reader callbacks run on SDK-managed background collection threads.
- Shared exporter state must be thread-safe; this example uses C11 atomics.
- Do not destroy handles concurrently with other calls on the same handle.

## Limitations

- Uses short sleeps only to cross interval boundaries and keep runtime deterministic.
- Does not demonstrate OTLP networking; it focuses on reader lifecycle behavior.
