<!-- SPDX-License-Identifier: Apache-2.0 -->

# c-basic-traces (split API + SDK example)

A C program that links against **both** `libopentelemetry_c_api` and
`libopentelemetry_c_sdk`. It plays the **application** role (build + install + flush +
shutdown the SDK) while emitting spans through the **API only** — exactly as an
instrumentation library would — demonstrating that API-only calls export through the
installed SDK.

The example emits three trace relationships:

- **Explicit parent context:** `handle-request` parents `query-database` using an immutable
  `otel_span_context_t` snapshot.
- **Ambient parent context:** `ambient-request` is placed in an `otel_context_t`, attached to
  the current thread, and discovered by `ambient-child` through `OTEL_PARENT_AMBIENT`.
- **W3C propagation:** `send-request` is injected into a `traceparent` carrier, extracted as a
  remote context, and used to parent `receive-request`, simulating a process boundary.

Ambient scopes are thread-local and must be detached in LIFO order on the attaching thread.
The W3C example uses an in-memory carrier for clarity; real applications copy the same header
bytes through HTTP, messaging, or another transport.

```sh
make run    # builds both Rust libs (release), links the example, runs it
```

By default it exports to `http://localhost:4318/v1/traces`; override with
`OTEL_EXPORTER_OTLP_TRACES_ENDPOINT`. Point it at an OpenTelemetry Collector (or any
OTLP/HTTP endpoint) to see the spans. If nothing is listening, export errors are logged but
the program still exits cleanly.
