# SPDX-License-Identifier: Apache-2.0

# Verify that PATH exists after a build step.
if(NOT EXISTS "${PATH}")
    message(FATAL_ERROR "Expected build artifact not found: ${PATH}")
endif()
