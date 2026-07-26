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
| `native-tls` | Yes | Implies `otlp`; OTLP HTTP/protobuf over the platform TLS backend. |
| `otlp` | Via `native-tls` | OTLP HTTP/protobuf trace and Metrics exporters. |
| `rustls-tls` | No | Implies `otlp`; OTLP HTTP/protobuf over rustls. |

The default is OTLP HTTP/protobuf with native TLS. Native TLS can require platform TLS or
OpenSSL development libraries. OTLP/gRPC and OTLP compression feature switches are not
present in this release line.

Recommended broad build using the alternative TLS backend:

```sh
cargo build --locked --release -p opentelemetry-c-sdk \
  --no-default-features --features otlp,rustls-tls
```

Smaller SDK-core build without OTLP or TLS:

```sh
cargo build --locked --release -p opentelemetry-c-sdk --no-default-features
```

Do not enable both TLS backends for a release build. Cargo features are compile-time
capabilities: selecting a C exporter option does not prove its implementation was compiled
in. Exporter construction returns `OTEL_STATUS_INVALID_CONFIG` when OTLP is unavailable.

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
