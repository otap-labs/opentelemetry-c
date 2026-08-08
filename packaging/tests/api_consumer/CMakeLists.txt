# SPDX-License-Identifier: Apache-2.0

cmake_minimum_required(VERSION 3.21)
project(otelc_api_consumer C)

find_package(OpenTelemetryC CONFIG REQUIRED)

add_executable(api_consumer main.c)
target_compile_features(api_consumer PRIVATE c_std_11)
if(MSVC)
    target_compile_options(api_consumer PRIVATE /W4 /WX)
else()
    target_compile_options(api_consumer PRIVATE -Wall -Wextra -Werror)
endif()
target_link_libraries(api_consumer PRIVATE OpenTelemetryC::api)

# Verify API consumer does NOT link SDK
get_target_property(_api_libs OpenTelemetryC::api INTERFACE_LINK_LIBRARIES)
if(_api_libs)
    foreach(_lib ${_api_libs})
        if("${_lib}" MATCHES "sdk")
            message(FATAL_ERROR "API consumer should not depend on SDK, but got: ${_lib}")
        endif()
    endforeach()
endif()

enable_testing()
add_test(NAME api_consumer_runs COMMAND api_consumer)
