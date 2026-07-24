# C Metrics example

This example builds an OTLP HTTP/protobuf metric exporter, a periodic reader, and an SDK
MeterProvider; installs it into the API-owned global Metrics slot; records synchronous and
observable metrics; flushes; and shuts down.

```sh
make run
```

Set `OTEL_EXPORTER_OTLP_METRICS_ENDPOINT` to override
`http://localhost:4318/v1/metrics`.

The observable handle may be destroyed before shutdown, which disables later callback work.
Its `user_data` destroy callback runs only after the Rust pipeline releases the registered
closure, normally during MeterProvider shutdown/drop.
