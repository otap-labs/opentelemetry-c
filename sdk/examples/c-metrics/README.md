# C Metrics example

This example builds an OTLP metric exporter, a periodic reader, and an SDK MeterProvider;
installs it into the API-owned global Metrics slot; records synchronous and observable
metrics; flushes; and shuts down. HTTP/protobuf remains the default:

```sh
make run
```

Set `OTEL_EXPORTER_OTLP_METRICS_ENDPOINT` to override
`http://localhost:4318/v1/metrics`.

For a library built with `otlp-grpc`, select gRPC explicitly:

```sh
OTEL_C_METRICS_TRANSPORT=grpc \
OTEL_EXPORTER_OTLP_METRICS_ENDPOINT=http://localhost:4317 \
make run
```

HTTP endpoints normally include `/v1/metrics`. gRPC endpoints are authorities; the
protobuf service supplies the RPC path. The SDK creates and owns the bounded Tokio runtime
needed by tonic, so the C application does not create or manage one.

The observable handle may be destroyed before shutdown, which disables later callback work.
Its `user_data` destroy callback runs exactly once after the handle is destroyed and after any
callback already in flight returns. It does not depend on MeterProvider shutdown or drop.
