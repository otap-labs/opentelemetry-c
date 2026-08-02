# SPDX-License-Identifier: Apache-2.0

cmake_minimum_required(VERSION 3.21)
project(opentelemetry_c_test_package C)

find_package(opentelemetry-c CONFIG REQUIRED)

add_executable(test_package main.c)
target_link_libraries(test_package PRIVATE opentelemetry-c::api)
