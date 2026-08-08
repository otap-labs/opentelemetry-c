<!-- SPDX-License-Identifier: Apache-2.0 -->

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

## Component maturity

Maturity is reported independently by signal and component in the root
[README](README.md#project-status). Trace, Metrics, and logs coverage can differ, and the
public C API, public native ABI, SDK, and exporters do not become stable merely because the
underlying OpenTelemetry Rust component or specification is stable.

Signals labelled **Experimental** in that table — currently Logs — sit below Alpha: their
headers, struct layouts, and exported symbols may change in any `0.x` release, including
patch releases, without the deprecation courtesy described above. Fields such as
`otel_log_record_view_t::struct_size` and the Logs vtable size/version exist so such changes
fail closed at run time rather than corrupting memory.

Feature coverage and compatibility maturity are separate claims. A surface described as
implemented or compliant may still be Alpha and subject to the pre-1.0 compatibility policy
above. The internal API-to-SDK ABI is version-checked for safe dispatch, but remains an
internal interface rather than a separately stable component.

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
- the SDK shared library records an ordinary native dependency on the matching API shared
  library; no process-global symbol lookup or special load order is required;
- both libraries remain loaded for the lifetime of every provider, tracer, span, meter,
  instrument, observable callback, global registration, and other handle that may call
  their code or vtables;
- neither library is unloaded after use; `dlclose` after API or SDK use is unsupported.

Linux and macOS shared-library use are supported. Windows DLL/import-library packaging is
implemented but remains experimental until it is continuously exercised on Windows CI.

## Static and mixed-link composition

An eventual supported all-static deployment must link exactly one API archive and one SDK
archive into the final executable. Every instrumentation object must resolve API calls to that
single API instance. A Cargo dependency from the SDK to the API crate is forbidden because it
can embed a private API rlib and duplicate the global provider slots.

The following mixed model is unsupported: an API archive embedded in an executable or plugin
combined with a dynamically loaded SDK shared library. It can either fail to load or silently
install into a different API instance. Exporting executable symbols does not make this a
supported composition.

The following configurations are unsupported:

- multiple statically linked API copies, which create independent global provider slots;
- a static API in an executable or plugin combined with a dynamically loaded SDK;
- any static deployment as a supported distribution model. Static libraries may remain
  buildable from source, but deployment is experimental and unsupported.

Using POSIX `fork()` without an immediate `exec()` after the SDK starts background workers
is unsupported. The child does not retain those worker threads or runtime in a usable state.

## Dependency and Rust toolchain policy

The Rust OpenTelemetry dependencies are implementation details. Updating them may change
behavior or require public C API changes, which follow the release rules above.

The minimum supported Rust version (MSRV) is a single workspace-wide product value shared by
all three project manifests (`opentelemetry-c-abi`, `opentelemetry-c-api`,
`opentelemetry-c-sdk`). The product MSRV covers only the shipped libraries built with the
committed `Cargo.lock`:

- the three crates built as libraries (`--lib`);
- the documented SDK production feature configurations, each checked with `--lib` and
  `--locked`: the default OTLP HTTP/protobuf build over the platform's native TLS
  (`--features` default, i.e. `native-tls`); the transport-free core
  (`--no-default-features`); the OTLP gRPC build (`--features otlp-grpc`); the combined OTLP
  HTTP and gRPC build over Rustls with native TLS disabled (`--features otlp-http,rustls-tls,
  otlp-grpc,grpc-tls-ring` plus the HTTP and gRPC compression features); and the all-features
  superset (`--all-features`), which enables every transport together with **both** HTTP TLS
  backends (`native-tls` and `rustls-tls`), all compression features, and the experimental
  async-runtime metrics reader.

The MSRV job runs this same locked, library-only matrix on both supported shared-library
platforms — Linux (`ubuntu-latest`) and macOS (`macos-latest`) — so target-specific
production dependencies (for example `openssl` on Linux and `security-framework` on macOS in
the native-TLS build) are proven to compile with the exact validated toolchain. Windows and
statically linked deployments are not supported and are not covered.

The product MSRV explicitly does **not** cover unit or integration tests, benchmarks, fuzz
targets, examples, developer tooling, or `rustfmt`/Clippy, nor the dependencies that only
those targets pull in. The committed `Cargo.lock` is part of the MSRV contract: the claim
holds for the locked dependency graph only, so consumers must build with `--locked` to obtain
the validated versions. Resolving dependencies afresh can select newer transitive crates that
require a newer compiler.

The MSRV is enforced by the dedicated `msrv` CI job, which is the authoritative proof of the
claim. That job reads the `OPENTELEMETRY_C_VALIDATED_MSRV` repository variable as the single
source of truth, verifies it equals the `rust-version` in all three manifests, installs that
exact toolchain (not latest stable), and runs the `--locked` library checks above on both
supported platforms. The job **fails closed** on every event while the variable is unset or
while it disagrees with the manifests, so a green pipeline never implies an unenforced MSRV.
Raising the MSRV requires two separate GitHub operations that must be kept consistent: a
commit that revalidates the new toolchain against this same locked matrix and updates the
`rust-version` in all three manifests, and a repository-variable update that sets
`OPENTELEMETRY_C_VALIDATED_MSRV` to the identical version. These are not a single atomic
change; until both agree the `msrv` job fails closed.

The product MSRV is **Rust 1.77.0**. This is the first Rust release that provides
`core::mem::offset_of!`, which the SDK's forward-compatible custom-exporter code uses in
`const` layout assertions and unsafe, size-gated member reads. Retaining the standard
`offset_of!` implementation avoids replacing audited ABI code merely to support two older
compiler releases.

The committed lockfile pins production transitive dependencies to releases that build with
Rust 1.77.0. In particular, an unconstrained resolution may select newer `indexmap`,
`hashbrown`, protocol, URL, or randomness dependencies whose own MSRVs are higher. These
pins are part of the locked source-release contract, not independent public compatibility
promises for those implementation dependencies.

Before the first release, set `OPENTELEMETRY_C_VALIDATED_MSRV` to `1.77.0`. See
[RELEASING.md](RELEASING.md).

## Stable compatibility

Before declaring a stable public ABI, the project will document:

- the exact source- and binary-compatibility guarantees;
- the supported platforms, architectures, and linking models;
- the deprecation and removal policy;
- compatibility expectations between separately installed API and SDK libraries.

Until then, pin the complete OpenTelemetry C release and review its changelogs before
upgrading.
