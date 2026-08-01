"""
Conan 2 test_package for opentelemetry-c.

Verifies that the installed package exposes the correct headers and that
a minimal C program links and runs against OpenTelemetryC::api.
"""

import os

from conan import ConanFile
from conan.tools.cmake import CMake, cmake_layout
from conan.tools.build import can_run


class OpenTelemetryCTestConan(ConanFile):
    settings = "os", "compiler", "build_type", "arch"
    generators = "CMakeDeps", "CMakeToolchain"

    def requirements(self):
        self.requires(self.tested_reference_str)

    def layout(self):
        cmake_layout(self)

    def build(self):
        cmake = CMake(self)
        cmake.configure()
        cmake.build()

    def test(self):
        if can_run(self):
            lib_dir = os.path.join(self.dependencies["opentelemetry-c"].package_folder, "lib")
            env = {}
            if self.settings.os == "Macos":
                env["DYLD_LIBRARY_PATH"] = lib_dir
            else:
                env["LD_LIBRARY_PATH"] = lib_dir
            self.run(os.path.join(self.cpp.build.bindir, "test_package"), env=env, run_environment=True)
