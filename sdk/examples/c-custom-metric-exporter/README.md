# c-custom-metric-exporter

Callback-based custom Metrics exporter with full batch traversal.

## What this example teaches

- Build a custom exporter with `export_metrics`, `force_flush`, `shutdown`, and `state_destroy`.
- Initialize versioned callback and visitor structs with `struct_size`.
- Traverse resource, scope, metric, point, attribute, and exemplar views.
- Distinguish metric data kinds and number kinds before reading unions.
- Keep callback state thread-safe for concurrent exporter callbacks.

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

A structured hierarchy similar to:

```text
resource
  scope
    metric
      point
        attributes
```

plus lifecycle lines for `force_flush`, `shutdown`, and `state_destroy`.

## Ownership and lifetime notes

- Exporter state ownership transfers to the exporter on successful construction.
- `state_destroy` is called exactly once after exporter callbacks are finished.
- All batch/visitor/metric/point/attribute/string/exemplar pointers are borrowed and
  callback-scoped; do not retain them after callback return.

## Threading notes

- Callbacks may run on SDK collection threads or force-flush caller threads.
- Different readers/SDKs may invoke the same callback state concurrently.
- This example serializes state and output with a mutex.

## Limitations

- Exemplar callback wiring is demonstrated, but exemplar emission depends on upstream SDK behavior.
