# opentelemetry-c-api

[![Apache License][license-image]][license-url]

The **C API facade** of the Rust-backed OpenTelemetry C binding. It exposes the public
trace, Metrics, and (experimental) Logs APIs as opaque C handles, **owns independent process-global provider
slots for each signal**, and ships **no-op defaults** so API-only instrumentation is safe
with or without an SDK.

This crate depends only on an internal ABI-types crate — never on `opentelemetry_sdk`,
`opentelemetry-otlp`, or `reqwest`.

> ⚠️ **Experimental.** The C ABI is not yet stable and may change between `0.x` releases.

## The API/SDK split

`opentelemetry-c` is split into two linkable artifacts:

| Library | Who links it | Contains |
| --- | --- | --- |
| **`libopentelemetry_c_api`** (this crate) | instrumentation **and** applications | trace + Metrics + Logs APIs, global provider slots, no-op defaults |
| **`libopentelemetry_c_sdk`** | applications only | OTLP exporters, processors/readers/views, SDK lifecycle |

- **Instrumentation libraries** link **only** `libopentelemetry_c_api`. Their trace,
  Metrics, and Logs calls are safe no-ops until an application installs the corresponding SDK
  provider, then they dispatch to it.
- **Applications** link **both** libraries. Installing the SDK
  (`otel_sdk_set_as_global`) registers it into *this* library's global provider slot
  (across the C ABI via the internal `otel_api_register_global_provider`), so it becomes
  visible to all API-only instrumentation.

There is exactly **one trace slot, one Metrics slot, and one Logs slot** in the process — all
owned here — so no duplicate global state exists across the two libraries. The three slots are
fully independent: installing or shutting down one signal never affects another.

### Logs (experimental)

Logs may be correlated either with the record's explicit trace/span identifiers or with an
immutable API-owned `otel_span_context_t` snapshot obtained from `otel_span_get_context()`.
Snapshots remain valid after the source span ends, are safe to clone/share across threads, can
parent a later span with `otel_tracer_start_span_with_context()`, and are borrowed only for
`otel_logger_emit_with_context()`. They are opaque, in-process handles in this release;
propagation import/export APIs will be designed with the future trace-context surface.

`logs.h` is a **log bridge**, meant for a logging library to route records through
OpenTelemetry. Records are described by a borrowed, one-shot `otel_log_record_view_t`;
structured values live in a flat node pool addressed by index range rather than a pointer
graph, so cycles cannot be expressed and an entire record can be validated without a visited
set. `event_name` and `target` are deliberately not exposed — see
[LOGS_COMPLIANCE.md](../LOGS_COMPLIANCE.md) for why and for the full constraint list.

### Linking & library lifetime (important)

The shared-global model is only guaranteed under **dynamic linking with exactly one loaded
`libopentelemetry_c_api`**:

- **Dynamic linking (supported model).** Instrumentation and the application resolve the
  same `libopentelemetry_c_api` at load time, so they share both global provider slots.
- **Static linking into multiple artifacts is *not* the shared-global model.** If
  `opentelemetry-c-api` is statically linked into more than one artifact (e.g. an
  instrumentation library *and* the application each statically embed it), each copy gets
  its **own** global provider slots and no-op defaults. An SDK installed into one copy
  is invisible to the other. Link the API as a single shared library so all callers observe
  the same slots.
- **Keep the SDK loaded while any installed provider or backed handle can dispatch to it.**
  The trace global remains installed until another trace provider replaces it, so trace
  installation still requires the SDK library to remain loaded for that window. Metrics
  installation receives a registration token: `otel_sdk_metrics_shutdown` and
  `otel_sdk_destroy` remove the Metrics global only if that SDK still owns the slot, without
  clearing a newer installation. Explicitly acquired trace/Metrics handles must also be
  destroyed while both libraries remain loaded. Unloading either library after use is
  unsupported.

## Headers

Under [`include/opentelemetry_c/`](include/opentelemetry_c):

- `common.h` — status codes, string views, typed attributes, version/error queries.
- `trace.h` — tracer provider, tracer, and span handles.
- `metrics.h` — typed synchronous/observable instruments, callbacks, and observations.
- `api.h` — umbrella (`common.h` + `trace.h` + `metrics.h`).

### Optional convenience helpers

Purely optional `static inline` (C99+/C++) wrappers over the raw API — **header-only, no ABI
symbols, no allocation or copy** (the status shorthands just perform the one
`otel_span_set_status()` call they wrap). String views are passed through borrowed, so the
referenced bytes must stay valid until the wrapped call returns. The public headers remain the
full reference:

- `otel_kv_string` / `otel_kv_bool` / `otel_kv_int64` / `otel_kv_double` — build a typed
  `otel_key_value_t` by value, e.g. for `otel_span_add_event()` attribute arrays.
- `otel_span_set_ok` / `otel_span_set_error` — optional status shorthands over
  `otel_span_set_status()`.
- `otel_cstr` / `otel_string_view_empty` — build a string view from a C string / an empty view.

## Building & linking

```sh
cargo build --release -p opentelemetry-c-api
```

This emits, under `target/release/`:

- a **shared library** (cdylib: `.so` / `.dylib`) — the artifact used by the supported
  dynamic-linking model;
- a **static library** (staticlib: `.a`) — see the static-linking caveat below;
- an `rlib` for Rust tests/internal use.

An **instrumentation library** compiles against the headers and links only the API:

```sh
cc -std=c11 my_instr.c \
   -I path/to/opentelemetry-c/api/include \
   -L path/to/target/release -lopentelemetry_c_api \
   -Wl,-rpath,path/to/target/release -o my_instr
```

Applications additionally link `libopentelemetry_c_sdk` — see that crate's README and the
`c-basic-traces` example.

**Static-linking caveat.** The static library is emitted, but supported static deployment
has not been designed or validated. Multiple API copies create independent global provider
slots, and a static API in an executable combined with a dynamically loaded SDK is
unsupported.

## Platform support

The dynamic API/SDK split (instrumentation links the API only; the SDK registers into the
API-owned global slot) is supported and continuously verified on **Unix-like dynamic
linking — Linux and macOS**. The cross-artifact proof test runs on both platforms in CI.

**Windows shared-library use is unsupported.** The SDK cdylib references the API cdylib's `otel_api_*`
symbols, which on Windows requires linking against the API's generated import library
(`.dll.lib`) rather than the load-time dynamic-lookup resolution used on Unix. Producing and
wiring that import library is follow-up work.

After either shared library has been used, unloading it with `dlclose` is unsupported.
Using `fork()` without an immediate `exec()` after SDK background workers start is also
unsupported.

## Ownership & safety

- Every handle-returning function transfers ownership; release with the matching
  `*_destroy`. Pass only NULL or a live project handle. A common raw prefix rejects a live
  handle of the wrong OpenTelemetry C type before full typed access. Foreign pointers,
  use-after-destroy, double destruction, and destruction races remain undefined behavior.
- Status classification is repository-wide: malformed immediate arguments use
  `OTEL_STATUS_INVALID_ARGUMENT`; readable but incompatible ABI/configuration or unavailable
  compiled support uses `OTEL_STATUS_INVALID_CONFIG`; bounded overruns use
  `OTEL_STATUS_TIMEOUT`; export/callback pipeline failures use `OTEL_STATUS_EXPORT_FAILED`;
  wrapper/infrastructure failures use `OTEL_STATUS_INTERNAL_ERROR`.
- Strings are borrowed `otel_string_view_t` values, copied before return.
- `otel_meter_provider_get_meter_with_options()` accepts a versioned complete
  instrumentation scope: name, version, schema URL, and uniquely keyed typed attributes.
  The API validates the borrowed data consistently even for API-only no-op meters; an
  SDK-backed meter copies it into the upstream `InstrumentationScope`.
- All entry points are panic-safe (a Rust panic is caught, never unwound into C).
- SDK/provider/tracer handles are safe to share across threads; a single span handle is
  not (one span per thread). `*_destroy` must not race with other calls on the same handle.

## License

Apache-2.0.

[license-image]: https://img.shields.io/badge/license-Apache_2.0-green.svg
[license-url]: https://github.com/open-telemetry/opentelemetry-rust-contrib/blob/main/LICENSE
