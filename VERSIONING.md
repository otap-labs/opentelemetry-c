# Versioning and compatibility

OpenTelemetry C is experimental. Until the project explicitly declares a stable release,
its public C API and ABI may change between `0.x` releases.

This document describes the intended compatibility policy during that experimental period.
It is not a promise of `1.0` stability.

## Releases

The `opentelemetry-c-api`, `opentelemetry-c-sdk`, and internal
`opentelemetry-c-abi` crates are developed and released as one coordinated component.
Consumers should use API and SDK libraries from the same release.

During `0.x`:

- minor releases may make source- or binary-incompatible changes;
- patch releases should remain source- and binary-compatible when practical;
- an incompatibility in a patch release must be called out prominently in the changelog;
- deprecated APIs may be removed in a later minor release without waiting for `1.0`.

The API and SDK changelogs record user-visible changes for their respective artifacts.

## Public C API and ABI

The installed C headers and exported `otel_*` symbols form the public interface. Public
compatibility includes:

- exported function names and calling conventions;
- fixed-width status codes and value representations;
- struct sizes, alignments, and field meanings;
- opaque-handle ownership and lifetime rules;
- documented threading and lifecycle behavior.

New functions and constants can normally be added compatibly. Existing public structures
must not be extended or reordered unless their contract explicitly provides a size or
version negotiation mechanism.

Opaque handles are intentionally used so implementations can evolve without exposing Rust
or SDK-internal layouts to C callers.

## Internal API-to-SDK ABI

The signal-specific trace and Metrics implementation vtables shared by the API and SDK
libraries are internal. They are not extension interfaces for applications or third-party
SDK implementations. Each vtable carries an ABI version and structure size; registration
rejects a mismatched version or undersized structure before storing or dispatching through
it.

API and SDK libraries from different releases are not supported unless a release explicitly
documents that combination as compatible. An internal compatibility check may reject a
mismatched pair rather than allowing unsafe dispatch.

## Dependency versions

The Rust OpenTelemetry dependencies are implementation details. Updating them may change
behavior or require public C API changes, which follow the release rules above.

The minimum supported Rust version is declared by each crate's `rust-version` field.

## Stable compatibility

Before declaring a stable public ABI, the project will document:

- the exact source- and binary-compatibility guarantees;
- the supported platforms, architectures, and linking models;
- the deprecation and removal policy;
- compatibility expectations between separately installed API and SDK libraries.

Until then, pin the complete OpenTelemetry C release and review its changelogs before
upgrading.
