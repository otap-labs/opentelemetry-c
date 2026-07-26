# Versioning, compatibility, and release policy

This document is the authoritative release policy for `opentelemetry-c`.

## Product identity

`opentelemetry-c` is one coordinated native C product consisting of the C API library, the
C SDK library, and the internal Rust ABI crate used by both. The project-owned Cargo
packages `opentelemetry-c-api`, `opentelemetry-c-sdk`, and `opentelemetry-c-abi`:

- use one product version;
- are released from one `vMAJOR.MINOR.PATCH` Git tag and one GitHub Release;
- are not independently released or supported;
- must not be mixed across releases.

The Cargo crates are implementation components used to build the C product. They are not
currently supported as independent Rust APIs and have `publish = false`.

## Experimental source-only releases

Before 1.0, releases are source-only:

- GitHub Releases use the repository tag's automatically generated `.tar.gz` and `.zip`;
- no native binaries, static/import libraries, header bundles, installers, or custom source
  archives are attached;
- checksums are not advertised for GitHub-generated archives because their byte
  representation is not guaranteed to remain stable;
- the project does not run `cargo publish` or publish placeholder crates;
- consumers build the API and SDK locally from the same tag.

`Cargo.lock` remains committed so a release tag selects the dependency versions validated
for that release. Maintainers must monitor dependency advisories and issue an updated
release when a relevant dependency vulnerability requires remediation.

## Pre-1.0 compatibility

The C API and ABI are experimental before 1.0:

- minor `0.x` releases may intentionally change C source or native ABI compatibility;
- patch releases preserve both C source compatibility and native ABI compatibility within
  their minor release;
- an incompatible change requires a new minor release;
- source compatibility is the primary practical promise because consumers build from
  source, but ABI compatibility still matters when replacing a library without recompiling;
- the project does not yet claim production readiness or a stable ABI.

Deprecated APIs may be removed in a later minor release. The coordinated API and SDK
changelogs distinguish changes to each artifact without implying independent releases.

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

The internal vtable ABI is reserved for this project and is not a supported third-party
extension interface.

## ABI crate invariant

The internal ABI crate may contain shared types, constants, layouts, and validation helpers.
It must not own process-global mutable state or define exported `#[no_mangle] extern "C"`
symbols. It is intentionally linked into both API and SDK artifacts; global state or
exported C symbols there could duplicate state or symbols across the libraries. The API
library is the sole owner of the process-global trace and Metrics provider slots.

## Supported shared-library model

The supported deployment model is:

- exactly one shared API library owns the process-global trace and Metrics provider slots;
- the matching SDK library registers providers through that API library;
- API and SDK are built from the same release;
- the API library is already present in the process's global symbol scope before the SDK is
  loaded, through normal application linking or platform-appropriate global loading;
- both libraries remain loaded for the lifetime of every provider, tracer, span, meter,
  instrument, observable callback, global registration, and other handle that may call
  their code or vtables;
- neither library is unloaded after use; `dlclose` after API or SDK use is unsupported.

Linux and macOS shared-library use are supported. Windows shared-library use is currently
unsupported because the SDK-to-API cross-library link mechanism has no Windows
implementation.

The following configurations are unsupported:

- multiple statically linked API copies, which create independent global provider slots;
- a static API in an executable combined with a dynamically loaded SDK;
- any static deployment as a supported distribution model. Static libraries may remain
  buildable from source, but deployment is experimental and unsupported.

Using POSIX `fork()` without an immediate `exec()` after the SDK starts background workers
is unsupported. The child does not retain those worker threads or runtime in a usable state.

## Dependency and Rust toolchain policy

The Rust OpenTelemetry dependencies are implementation details. Updating them may change
behavior or require public C API changes, which follow the release rules above.

All three project manifests must declare the same validated minimum supported Rust version
(MSRV), and release CI must check the product libraries and documented feature combinations
with it. Test, integration-test, and benchmark-only dependencies do not define the product
MSRV. The committed dependency set used by the product libraries is part of that claim.

The current `rust-version = "1.75.0"` declarations are not yet a verified release promise:
the committed lockfile contains dependencies that require newer Cargo/Rust language
support. The first source release is blocked until maintainers either pin a dependency set
that builds on Rust 1.75 and enforce it in CI, or choose and validate a higher product MSRV.
Until then, the MSRV CI build is gated rather than running a known-incompatible toolchain.
See [RELEASING.md](RELEASING.md).

## Stable compatibility

Before declaring a stable public ABI, the project will document:

- the exact source- and binary-compatibility guarantees;
- the supported platforms, architectures, and linking models;
- the deprecation and removal policy;
- compatibility expectations between separately installed API and SDK libraries.

Until then, pin the complete OpenTelemetry C release and review its changelogs before
upgrading.
