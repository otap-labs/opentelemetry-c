# opentelemetry-c

A **Rust-backed C API/SDK for OpenTelemetry traces** — experimental C ABI bindings that
expose the Rust OpenTelemetry implementation through a stable-ish C ABI so C/C++
instrumentation and other runtimes can consume it without binding to Rust or C++ directly.

> ⚠️ **Experimental.** The C ABI is not yet stable and may change between `0.x` releases.

The implementation lands via the initial pull request. See the PR for the full component
(`api/`, `sdk/`, `abi/`), C headers, examples, and tests.
