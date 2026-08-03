# SPDX-License-Identifier: Apache-2.0

"""
Conan 2 local recipe for opentelemetry-c.

This is a LOCAL recipe; it builds directly from the source tree rather than
downloading a tarball. Intended for development-time consumption via a Conan
local cache overlay (`conan export` or `conan create`).

Usage (from repository root):
    conan export packaging/conan --name opentelemetry-c --version 0.1.0
    conan install --requires="opentelemetry-c/0.1.0" --build=opentelemetry-c

NOTE: Prebuilt binaries are NOT published. This recipe is provided for
consumers who manage C++ dependencies with Conan 2.
"""

import os
import shutil

from conan import ConanFile
from conan.errors import ConanInvalidConfiguration
from conan.tools.cmake import CMake, CMakeToolchain, cmake_layout
from conan.tools.files import copy


class OpenTelemetryCConan(ConanFile):
    name = "opentelemetry-c"
    version = "0.1.0"
    description = "OpenTelemetry C API and SDK – Rust-backed C bindings"
    homepage = "https://github.com/otap-labs/opentelemetry-c"
    license = "Apache-2.0"
    topics = ("opentelemetry", "tracing", "metrics", "logs", "observability")

    settings = "os", "compiler", "build_type", "arch"
    # Shared-only: static deployment is not supported (see README.md)
    options = {
        "otlp_http": [True, False],
        "otlp_grpc": [True, False],
        "native_tls": [True, False],
        "rustls_tls": [True, False],
        "metrics_async_runtime": [True, False],
        "no_default_features": [True, False],
    }
    default_options = {
        "otlp_http": False,
        "otlp_grpc": False,
        "native_tls": True,
        "rustls_tls": False,
        "metrics_async_runtime": False,
        "no_default_features": False,
    }

    # No exports_sources class attribute: we override export_sources() below to
    # copy from the repository root (two levels up from packaging/conan/).

    def export_sources(self):
        # The recipe lives at packaging/conan/conanfile.py; the source tree is
        # the repository root two levels up.
        repo_root = os.path.normpath(os.path.join(os.path.dirname(__file__), "..", ".."))
        dst = self.export_sources_folder
        for pattern in [
            "CMakeLists.txt", "Cargo.toml", "Cargo.lock",
            "LICENSE", "README.md", "VERSIONING.md", "RELEASING.md",
            "SECURITY.md", "*_COMPLIANCE.md",
        ]:
            copy(self, pattern, src=repo_root, dst=dst)
        for subdir in ["cmake", "api", "sdk", "abi", "docs", "scripts"]:
            copy(self, f"{subdir}/*", src=repo_root, dst=dst)

    def validate(self):
        if self.settings.os == "Windows":
            raise ConanInvalidConfiguration(
                "opentelemetry-c does not support Windows shared-library deployment."
            )
        if self.options.native_tls and self.options.rustls_tls:
            raise ConanInvalidConfiguration(
                "native_tls and rustls_tls are mutually exclusive."
            )

    def layout(self):
        cmake_layout(self)

    def generate(self):
        tc = CMakeToolchain(self)
        tc.variables["OTEL_SDK_OTLP_HTTP"] = self.options.otlp_http
        tc.variables["OTEL_SDK_OTLP_GRPC"] = self.options.otlp_grpc
        tc.variables["OTEL_SDK_NATIVE_TLS"] = self.options.native_tls
        tc.variables["OTEL_SDK_RUSTLS_TLS"] = self.options.rustls_tls
        tc.variables["OTEL_SDK_METRICS_ASYNC_RUNTIME"] = self.options.metrics_async_runtime
        tc.variables["OTEL_SDK_NO_DEFAULT_FEATURES"] = self.options.no_default_features
        tc.generate()

    def build(self):
        cmake = CMake(self)
        cmake.configure()
        cmake.build()

    def package(self):
        cmake = CMake(self)
        cmake.install()
        copy(self, "LICENSE", self.source_folder, os.path.join(self.package_folder, "licenses"))

    def package_info(self):
        self.cpp_info.components["api"].libs = ["opentelemetry_c_api"]
        self.cpp_info.components["api"].includedirs = ["include"]
        self.cpp_info.components["api"].set_property("pkg_config_name", "opentelemetry-c-api")

        self.cpp_info.components["sdk"].libs = ["opentelemetry_c_sdk"]
        self.cpp_info.components["sdk"].includedirs = ["include"]
        self.cpp_info.components["sdk"].requires = ["api"]
        self.cpp_info.components["sdk"].set_property("pkg_config_name", "opentelemetry-c-sdk")

        if self.settings.os in ("Linux", "FreeBSD"):
            self.cpp_info.components["api"].system_libs = ["dl", "pthread"]
            self.cpp_info.components["sdk"].system_libs = ["dl", "pthread"]
