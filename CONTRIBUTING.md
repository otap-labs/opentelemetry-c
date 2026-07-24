# Contributing

OpenTelemetry C is an experimental Rust-backed C API and SDK. Contributions are welcome,
especially those that improve FFI safety, interoperability, documentation, and the planned
traces, metrics, and logs surfaces.

## Before starting

For a significant API, ABI, lifecycle, or architecture change, open or use an existing issue
before implementation. Describe the C-facing use case, proposed ownership rules, and expected
hot-path cost. Small fixes, tests, and documentation improvements do not need a design issue.

Keep changes focused. Traces, metrics, and logs are tracked as separate initiatives and can
evolve incrementally.

## Development requirements

- Use a Rust toolchain at or above the `rust-version` declared in the crate manifests.
- Public headers must compile as C and C++ where documented.
- Do not expose Rust layouts, Rust enums, or Rust-owned references directly through the C ABI.
- Preserve the Apache-2.0 license headers and project license.

## Design rules

All exported C functions must:

- use C-compatible, explicitly documented types;
- validate caller-provided pointers, lengths, and discriminants before use;
- prevent Rust panics from crossing the FFI boundary;
- document ownership transfer, destruction, lifetime, and thread-safety rules;
- return a defined status and useful last-error diagnostic on failure.

Public discriminants and status values should use fixed-width integer representations.
Unknown values must be rejected safely unless the contract explicitly gives them another
meaning.

Prefer opaque handles for objects with evolving internal state. A successful ownership
transfer must have exactly one owner; failure should leave ownership with the caller unless
the function contract says otherwise.

The internal API-to-SDK vtable is append-only within a compatible ABI version. Changes must
preserve its version/size validation contract, or deliberately increment the internal ABI
version when compatibility cannot be maintained.

Telemetry hot paths must remain a thin FFI boundary. Avoid adding global locks, registries,
configuration lookups, exporter access, or unrelated allocation to per-span, per-log-record,
or per-measurement calls. Include benchmark evidence when a change could materially affect
CPU time or memory use.

See [VERSIONING.md](VERSIONING.md) for the current compatibility policy.

## Building and testing

Format and lint the workspace:

```sh
./scripts/lint.sh
```

Build and test the default and SDK-core configurations:

```sh
./scripts/test.sh
```

The scripts currently cover:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo build --workspace --all-targets
cargo test --workspace --all-targets
cargo test -p opentelemetry-c-sdk --no-default-features
```

Add tests at the level affected by the change:

- Rust unit tests for validation and lifecycle behavior;
- C header compile tests for public declarations and C/C++ compatibility;
- cross-artifact tests for behavior between separate API and SDK libraries;
- benchmarks for hot-path changes.

Benchmarks are opt-in and are not part of the default test script:

```sh
cargo bench -p opentelemetry-c-api
cargo bench -p opentelemetry-c-sdk
```

## Documentation and changelogs

Update public headers and the relevant README when changing caller-visible behavior. Keep
cross-cutting usage guidance in the root README and implementation-specific details in the
API or SDK README.

Add user-visible API changes to `api/CHANGELOG.md` and SDK changes to
`sdk/CHANGELOG.md`. ABI breaks and migration requirements must be clearly identified.

## Pull requests

A pull request should explain:

- what changed and why;
- any API, ABI, ownership, lifecycle, or performance impact;
- the validation performed;
- known limitations or intentionally deferred work.

Experimental and iterative pull requests are acceptable when their scope and follow-up work
are explicit.
