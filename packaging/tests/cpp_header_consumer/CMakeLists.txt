# SPDX-License-Identifier: Apache-2.0

cmake_minimum_required(VERSION 3.21)
project(otelc_cpp_consumer CXX)

find_package(OpenTelemetryC CONFIG REQUIRED)

add_executable(cpp_consumer main.cpp)
target_compile_features(cpp_consumer PRIVATE cxx_std_17)
if(MSVC)
    target_compile_options(cpp_consumer PRIVATE /W4 /WX)
else()
    target_compile_options(cpp_consumer PRIVATE -Wall -Wextra -Werror)
endif()
target_link_libraries(cpp_consumer PRIVATE OpenTelemetryC::api)

enable_testing()
add_test(NAME cpp_consumer_runs COMMAND cpp_consumer)
