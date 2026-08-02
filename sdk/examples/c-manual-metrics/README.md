<!-- SPDX-License-Identifier: Apache-2.0 -->

# c-manual-metrics

Deterministic, application-controlled Metrics collection with the public manual reader.

## What this example teaches

- Build a custom callback exporter and `otel_manual_metric_reader_t`.
- Add the manual reader to the SDK and install global Metrics.
- Record measurements and trigger export exactly at checkpoint boundaries.
- Use `otel_sdk_metrics_force_flush` as the collection trigger for manual readers.

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
manual reader exported 2 batches; last cumulative value=7
```

## Ownership and lifetime notes

- `otel_manual_metric_reader_new` consumes the exporter on success.
- `otel_sdk_builder_add_manual_metric_reader` consumes the reader on success.
- `otel_metric_batch_visit` views are borrowed and valid only during the callback.

## Threading notes

- Manual readers do not run background collection threads.
- Collection/export runs on the thread that calls `otel_sdk_metrics_force_flush`.

## Limitations

- There is no separate public `collect()` operation in the C API today.
- `timeout_millis` on `otel_sdk_metrics_force_flush` is currently advisory for Metrics.

## Good use cases

- Unit/integration tests.
- CLI tools and batch jobs.
- Embedded or offline programs.
- Explicit checkpoint-based collection.
