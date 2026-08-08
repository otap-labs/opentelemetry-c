// SPDX-License-Identifier: Apache-2.0

//! Build script for `opentelemetry-c-sdk`.
//!
//! The SDK cdylib references the API cdylib's internal registration symbols
//! (`otel_api_register_global_provider`, `otel_api_provider_new`, `otel_api_set_last_error`,
//! `otel_api_clear_last_error`). The native linker records the matching API shared library
//! as a dependency of the SDK cdylib.
//!
//! The SDK cdylib has an ordinary native dependency on the API shared library. This is
//! intentionally a linker-level dependency, not a Cargo dependency on the API crate: the
//! latter could embed a second API rlib and duplicate its process-global provider slots.

fn main() {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    let link_dir = std::env::var_os("OTEL_C_API_LINK_DIR")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            // OUT_DIR is <target>/<profile>/build/<package-hash>/out.
            let out = std::path::PathBuf::from(std::env::var_os("OUT_DIR")?);
            out.ancestors().nth(3).map(std::path::Path::to_path_buf)
        })
        .expect("cannot determine API shared-library directory");

    println!("cargo:rerun-if-env-changed=OTEL_C_API_LINK_DIR");
    match target_os.as_str() {
        "windows" => {
            assert_eq!(
                target_env, "msvc",
                "experimental Windows packaging currently supports only the MSVC target"
            );
            // rustc's MSVC cdylib output includes this import library beside the DLL.
            println!(
                "cargo:rustc-cdylib-link-arg={}",
                link_dir.join("opentelemetry_c_api.dll.lib").display()
            );
        }
        "macos" | "ios" => {
            println!("cargo:rustc-cdylib-link-arg=-L{}", link_dir.display());
            println!("cargo:rustc-cdylib-link-arg=-lopentelemetry_c_api");
            println!("cargo:rustc-cdylib-link-arg=-Wl,-rpath,@loader_path");
            println!(
                "cargo:rustc-cdylib-link-arg=-Wl,-install_name,@rpath/libopentelemetry_c_sdk.dylib"
            );
        }
        _ => {
            println!("cargo:rustc-cdylib-link-arg=-L{}", link_dir.display());
            println!("cargo:rustc-cdylib-link-arg=-lopentelemetry_c_api");
            println!("cargo:rustc-cdylib-link-arg=-Wl,-rpath,$ORIGIN");
        }
    }
}
