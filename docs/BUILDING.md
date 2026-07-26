# Building from a source release

`opentelemetry-c` is distributed as source. API and SDK libraries must be built from the
same `vMAJOR.MINOR.PATCH` tag and kept loaded while OpenTelemetry handles can use them.

## Build the libraries

1. Download a GitHub-generated source archive for a release tag, or clone and check out the
   tag.
2. Install the Rust toolchain listed as supported by that release.
3. From the repository root, build the API:

   ```sh
   cargo build --locked --release -p opentelemetry-c-api
   ```

4. Choose SDK features and build the SDK. The default build is:

   ```sh
   cargo build --locked --release -p opentelemetry-c-sdk
   ```

Cargo writes the libraries to `target/release/`. On Linux, expect
`libopentelemetry_c_api.so` and `libopentelemetry_c_sdk.so`; on macOS, expect
`libopentelemetry_c_api.dylib` and `libopentelemetry_c_sdk.dylib`. Cargo also emits static
libraries, but supported static deployment has not been designed or validated.

## SDK feature selection

The current `sdk/Cargo.toml` feature graph is:

| Feature | Default | Capability |
| --- | --- | --- |
| `native-tls` | Yes | Implies `otlp-http`; HTTP HTTPS through the platform TLS backend. |
| `otlp` | No | Compatibility alias that enables `otlp-http`. |
| `otlp-http` | Via `native-tls` | OTLP HTTP/protobuf trace and Metrics exporters. |
| `otlp-grpc` | No | OTLP/gRPC Metrics using tonic and an SDK-owned Tokio runtime. |
| `metrics-async-runtime` | No | SDK-owned async periodic Metrics reader with configurable export timeout. |
| `rustls-tls` | No | Implies `otlp-http`; HTTP HTTPS through rustls. |
| `grpc-tls-ring` | No | Implies `otlp-grpc`; gRPC TLS using ring and platform roots. |
| `otlp-http-gzip` | No | HTTP gzip compression; implies `otlp-http`. |
| `otlp-http-zstd` | No | HTTP zstd compression; implies `otlp-http`. |
| `otlp-grpc-gzip` | No | gRPC gzip compression; implies `otlp-grpc`. |
| `otlp-grpc-zstd` | No | gRPC zstd compression; implies `otlp-grpc`. |

The default is OTLP HTTP/protobuf with native TLS. Native TLS can require platform TLS or
OpenSSL development libraries. OTLP/gRPC is opt-in.

Recommended broad build using Rustls for HTTP and ring TLS for gRPC:

```sh
cargo build --locked --release -p opentelemetry-c-sdk \
  --no-default-features \
  --features otlp-http,rustls-tls,otlp-grpc,grpc-tls-ring,otlp-http-gzip,otlp-http-zstd,otlp-grpc-gzip,otlp-grpc-zstd
```

Smaller plaintext gRPC Metrics build:

```sh
cargo build --locked --release -p opentelemetry-c-sdk \
  --no-default-features --features otlp-grpc
```

An SDK-core-only build remains available with `--no-default-features`. Do not enable both
HTTP TLS backends (`native-tls` and `rustls-tls`) for a release build. Cargo features are
compile-time capabilities: selecting a C transport or compression enum does not prove that
capability was compiled in. Exporter construction returns `OTEL_STATUS_INVALID_CONFIG` and
names the required feature when the selected transport or compression is unavailable.

The periodic Metrics reader remains blocking by default. Enabling
`metrics-async-runtime` allows C code to select an SDK-owned async reader with one Tokio
worker and at most one blocking thread; applications never provide a Rust runtime. The async
reader currently supports custom exporters. It rejects the blocking OTLP/HTTP exporter and the
synchronous OTLP/gRPC wrapper because neither can be safely driven inside its Tokio worker. Its
reader timeout is cooperative and cannot interrupt synchronous custom callback work, so callback
implementations must remain bounded.

## Compile and link a C application

SDK headers include API headers using `<opentelemetry_c/...>` paths, so both source include
roots are required:

```sh
cc -std=c11 my_app.c \
  -I<repo>/api/include \
  -I<repo>/sdk/include \
  -L<repo>/target/release \
  -lopentelemetry_c_api -lopentelemetry_c_sdk \
  -Wl,-rpath,<repo>/target/release \
  -o my_app
```

Link or globally load the API before the SDK. Keep both libraries loaded for the lifetime
of all providers, tracers, spans, meters, instruments, callbacks, registrations, and other
OpenTelemetry handles. `dlclose` after use is unsupported.

The trace and Metrics examples provide working Makefiles:

- [`sdk/examples/c-basic-traces`](../sdk/examples/c-basic-traces)
- [`sdk/examples/c-metrics`](../sdk/examples/c-metrics)

Windows shared-library use and supported static deployment are not currently available.
