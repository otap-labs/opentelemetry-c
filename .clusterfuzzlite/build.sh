#!/bin/bash -eu
# SPDX-License-Identifier: Apache-2.0

cd "$SRC/opentelemetry-c"

# ClusterFuzzLite's Rust builder supplies nightly Rust, cargo-fuzz, libFuzzer, and the
# requested sanitizer instrumentation. Keep this build aligned with fuzz/Cargo.toml instead
# of maintaining a second target list here.
cargo fuzz build -O

fuzz_target_dir="fuzz/target/x86_64-unknown-linux-gnu/release"
for target_source in fuzz/fuzz_targets/*.rs; do
    target_name="$(basename "${target_source%.rs}")"
    cp "$fuzz_target_dir/$target_name" "$OUT/$target_name"
done
