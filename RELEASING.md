# Releasing

Releases are coordinated, experimental, and source-only. Follow
[VERSIONING.md](VERSIONING.md).

1. Confirm the intended `MAJOR.MINOR.PATCH` version and that the product MSRV is validated
   with the committed lockfile.
2. Set the same version in `api/Cargo.toml`, `sdk/Cargo.toml`, and `abi/Cargo.toml`.
3. Update the API and SDK changelogs under the same release heading.
4. Update `Cargo.lock` only as required by legitimate manifest version changes.
5. Confirm every project package still has `publish = false`.
6. Confirm SDK feature documentation exactly matches `sdk/Cargo.toml`.
7. Note that `otel_version_major()`, `otel_version_minor()`, `otel_version_patch()`, and
   `otel_version_string()` derive automatically from the API package version; there are no
   public version macros to edit.
8. Run normal CI, the blocking MSRV check, dependency advisory audit, examples, and
   supported-platform validation.
9. Triage every advisory. A temporary exception must be documented with the advisory ID,
   impact analysis, owner, and removal condition; do not silently ignore advisories.
10. Run the release metadata check:

    ```sh
    scripts/check-release-metadata.sh
    scripts/check-release-metadata.sh v0.x.y
    ```

11. Create an annotated `v0.x.y` tag and one GitHub Release from it.
12. Use only GitHub-generated source `.tar.gz` and `.zip` archives. Do not attach native
    binaries, create a custom archive, advertise archive checksums, or run `cargo publish`.
13. Include experimental status, supported platforms, SDK features, compatibility policy,
    source-build instructions, and known limitations in the release notes.
14. Verify the tagged source contains `Cargo.lock`, API and SDK headers, `LICENSE`,
    `README.md`, `VERSIONING.md`, `RELEASING.md`, both changelogs, and examples.

The first release remains blocked until the MSRV and private security-reporting channel are
resolved and enforced. After choosing and validating the MSRV, update all three manifests
and set the `OPENTELEMETRY_C_VALIDATED_MSRV` repository variable to the exact same version;
the dedicated CI job then checks the locked workspace with that toolchain.
