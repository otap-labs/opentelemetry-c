# c-metrics-basic

Smallest complete Metrics SDK program using only public C headers.

## What this example teaches

- Build a callback-based Metrics exporter and manual reader.
- Build/install an SDK MeterProvider.
- Create one counter and record low-cardinality attributes.
- Trigger collection/export with `otel_sdk_metrics_force_flush`.
- Shut down cleanly and release every owned handle.

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

A success line similar to:

```text
basic metrics example exported 1 batch(es)
```

## Ownership and lifetime notes

- `otel_manual_metric_reader_new` consumes the exporter on `OTEL_STATUS_OK`.
- `otel_sdk_builder_add_manual_metric_reader` consumes the reader on `OTEL_STATUS_OK`.
- `otel_sdk_metrics_shutdown` triggers exporter shutdown; `state_destroy` runs exactly once.
- Data seen through `otel_metric_batch_visit` callbacks is borrowed and callback-scoped.

## Threading notes

- This example runs deterministically on one thread with a manual reader.
- Exporter callbacks still need thread-safe state in general when shared across readers/SDKs.

## Limitations

- Demonstrates one synchronous instrument only. See `c-metrics-instruments` for broader coverage.
