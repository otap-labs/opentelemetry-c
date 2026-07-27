/*
 * opentelemetry_c/api.h
 *
 * Umbrella header for the OpenTelemetry **C API** library (`libopentelemetry_c_api`).
 * Include this to pull in the full trace API surface used by instrumentation:
 *
 *   #include <opentelemetry_c/api.h>       // common.h + logs.h + metrics.h + trace.h
 *
 * or the individual headers:
 *
 *   #include <opentelemetry_c/common.h>    // status, string views, attributes, version
 *   #include <opentelemetry_c/logs.h>      // logger provider, logger, log records
 *   #include <opentelemetry_c/metrics.h>   // meter provider, meter, instruments
 *   #include <opentelemetry_c/trace.h>     // tracer provider, tracer, span
 *
 * Linking model
 * -------------
 *   - Instrumentation libraries link ONLY `libopentelemetry_c_api`. Trace calls are safe
 *     no-ops until an SDK is installed, then dispatch to it.
 *   - Applications additionally link `libopentelemetry_c_sdk` and include
 *     <opentelemetry_c/sdk.h> to build and install an SDK. The SDK registers into this
 *     library's single global provider slot, so it is visible to API-only instrumentation.
 *
 * Library lifetime
 * ----------------
 *   - The shared global provider is guaranteed ONLY under dynamic linking with exactly one
 *     loaded `libopentelemetry_c_api`. Statically linking this library into more than one
 *     artifact gives each copy its OWN global slot and no-op default (not the shared model).
 *   - After either API or SDK library has been used, both must remain loaded until process
 *     exit. Replacing providers, shutdown, and handle destruction do NOT make dlclose
 *     supported.
 *
 * Platform status
 * ---------------
 * The shared-global model is verified on Unix-like dynamic linking (Linux and macOS).
 * Windows shared-library use is unsupported — the SDK's undefined otel_api_*
 * symbols need the API's import library at link time (follow-up work); see README.md.
 *
 * Status: EXPERIMENTAL. The C ABI is not yet stable (see README.md).
 */
#ifndef OPENTELEMETRY_C_API_H
#define OPENTELEMETRY_C_API_H

#include "common.h"
#include "logs.h"
#include "metrics.h"
#include "trace.h"

#endif /* OPENTELEMETRY_C_API_H */
