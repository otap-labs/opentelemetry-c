# SPDX-License-Identifier: Apache-2.0

.PHONY: all run clean
API_INC := ../../../api/include
LIB_DIR := ../../../target/release
BINARY := c-baggage
CC ?= cc
CFLAGS ?= -std=c11 -Wall -Wextra -Werror -O2
UNAME_S := $(shell uname -s)
ifeq ($(UNAME_S),Darwin)
RPATH := -Wl,-rpath,@loader_path/$(LIB_DIR)
else
RPATH := -Wl,-rpath,$$ORIGIN/$(LIB_DIR)
endif
all: $(BINARY)
build-rust:
	@cd ../../.. && cargo build --release -p opentelemetry-c-api
$(BINARY): main.c worker.c build-rust
	$(CC) $(CFLAGS) -I$(API_INC) main.c worker.c -L$(LIB_DIR) -lopentelemetry_c_api $(RPATH) -o $(BINARY)
run: $(BINARY)
	@DYLD_LIBRARY_PATH=$(LIB_DIR) LD_LIBRARY_PATH=$(LIB_DIR) ./$(BINARY)
clean:
	@rm -f $(BINARY)
