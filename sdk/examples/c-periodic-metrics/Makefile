# SPDX-License-Identifier: Apache-2.0

.PHONY: all run clean

API_INC := ../../../api/include
SDK_INC := ../../include
LIB_DIR := ../../../target/release
BINARY := c-periodic-metrics

CC ?= cc
CSTD ?= -std=c11
CFLAGS ?= -Wall -Wextra -O2

UNAME_S := $(shell uname -s)
ifeq ($(UNAME_S),Darwin)
	RPATH := -Wl,-rpath,@loader_path/$(LIB_DIR)
else
	RPATH := -Wl,-rpath,$$ORIGIN/$(LIB_DIR)
endif

all: $(BINARY)

build-rust:
	@cd ../../.. && cargo build --release -p opentelemetry-c-api -p opentelemetry-c-sdk

$(BINARY): main.c build-rust
	@$(CC) $(CSTD) $(CFLAGS) -o $(BINARY) main.c \
		-I$(API_INC) -I$(SDK_INC) -L$(LIB_DIR) \
		-lopentelemetry_c_api -lopentelemetry_c_sdk $(RPATH)

run: $(BINARY)
	@DYLD_LIBRARY_PATH=$(LIB_DIR) LD_LIBRARY_PATH=$(LIB_DIR) ./$(BINARY)

clean:
	@rm -f $(BINARY)
