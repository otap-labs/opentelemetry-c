// SPDX-License-Identifier: Apache-2.0

//! Platform linker identity for the API shared library.

fn main() {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os == "macos" || target_os == "ios" {
        // Installed SDKs refer to the API by loader-relative rpath, never by Cargo's build path.
        println!(
            "cargo:rustc-cdylib-link-arg=-Wl,-install_name,@rpath/libopentelemetry_c_api.dylib"
        );
    }
}
