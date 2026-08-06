# SPDX-License-Identifier: Apache-2.0

cmake_minimum_required(VERSION 3.21)
project(otelc_sdk_consumer C)

find_package(OpenTelemetryC CONFIG REQUIRED)

add_executable(sdk_consumer main.c)
target_compile_features(sdk_consumer PRIVATE c_std_11)
if(MSVC)
    target_compile_options(sdk_consumer PRIVATE /W4 /WX)
else()
    target_compile_options(sdk_consumer PRIVATE -Wall -Wextra -Werror)
endif()
target_link_libraries(sdk_consumer PRIVATE OpenTelemetryC::sdk)

enable_testing()
add_test(NAME sdk_consumer_runs COMMAND sdk_consumer)
