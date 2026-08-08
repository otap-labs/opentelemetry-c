<!-- SPDX-License-Identifier: Apache-2.0 -->

# c-metrics-instruments

Demonstrates supported public Metrics instrument kinds with practical meanings.

## What this example teaches

- Counter (`requests.completed`): monotonic completed work.
- Up-down counter (`connections.active`): values can increase and decrease.
- Gauge (`queue.depth`): latest observed level.
- Histogram (`request.duration.ms`): distribution of measurements over buckets.
- Bound instruments (counter + histogram): the recommended zero-allocation recording path for
  hot loops that repeatedly use one fixed attribute set.
- Observable instruments (counter/up-down/gauge): callback-based measurements during collection.

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

Metric names and kinds printed from exporter callbacks, then a summary line:

```text
instruments example exported <n> batch(es) with <m> metric callbacks
```

## Ownership and lifetime notes

- Bound handles are independent owned handles; destroy them explicitly.
- Binding performs attribute validation and ownership conversion once. Prefer ordinary
  instruments when attributes vary for every measurement.
- Observable creation transfers callback user-data ownership on success.
- Destroying observable handles disables future callback work and releases owned state.

## Threading notes

- Observable callbacks run during reader collection and may be invoked concurrently by
  different readers/SDKs.
- Observer tokens are callback-thread-local and callback-scoped.

## Limitations

- Keeps attributes intentionally low-cardinality (`endpoint`, `region`, `source`).
- Focuses on instrument semantics, not deep batch traversal internals.
