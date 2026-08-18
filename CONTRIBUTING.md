<!-- SPDX-License-Identifier: Apache-2.0 -->

# Contributing

This repository produces one coordinated C product. The API, SDK, and internal ABI Cargo
packages share one version and are not independently published.

- Public C API or ABI changes require matching headers, implementation, tests, changelog,
  versioning, ownership/lifecycle documentation, and compatibility review.
- The internal ABI crate must remain free of process-global mutable state and exported
  `#[no_mangle] extern "C"` symbols.
- SDK feature changes must update source-build documentation and feature validation.
- Dependency changes require deliberate review because releases ship the committed
  `Cargo.lock`. Dependency advisory checks are intentionally blocking; if an advisory cannot
  be fixed immediately, document its ID, impact, owner, and removal condition as described in
  [RELEASING.md](RELEASING.md) rather than silently ignoring it.
- Do not add the API crate as a normal Rust dependency of the SDK; cross-library
  registration intentionally uses external C symbols.
- Do not commit generated native binaries. Binary packaging, installers, and supported
  static distributions remain out of scope.

Run the repository's existing formatting, linting, and test scripts for code changes. For
release-policy changes, also run `scripts/check-release-metadata.sh`.

## License identifiers

Original source, configuration, scripts, and documentation must include the
machine-readable identifier `SPDX-License-Identifier: Apache-2.0` using the file format's
comment syntax. Do not add headers to generated files, lockfiles, binary or fuzz-corpus data,
JSON files, or content-sensitive symbol inventories. Run
`scripts/check-license-headers.py --fix` to add missing identifiers to eligible files.
