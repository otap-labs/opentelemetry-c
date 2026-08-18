// SPDX-License-Identifier: Apache-2.0

/*
 * opentelemetry_c/sdk.h
 *
 * SDK configuration and lifecycle: build tracer, meter, and (experimental) logger providers
 * with OTLP exporters and batch processors, install them globally, flush, and shut down.
 * Each signal has its own independent global slot and lifecycle.
 *
 * The SDK owns all of its own threading (dedicated batch-processor OS threads and the
 * blocking HTTP client). No user-managed async runtime is required. Metrics reader
 * collection may invoke observable and custom-exporter C callbacks on SDK-managed
 * collection threads or, for a manual reader, on the force-flush caller's thread.
 *
 * Threading & lifecycle contract
 * ------------------------------
 *   - An otel_sdk_t handle may be used concurrently from multiple threads:
 *     otel_sdk_set_as_global(), otel_sdk_force_flush(), otel_sdk_shutdown(), and
 *     otel_sdk_get_tracer_provider() are all safe to call at the same time on one handle.
 *   - otel_sdk_shutdown() runs the underlying shutdown at most once. The first call wins;
 *     concurrent or later calls return OTEL_STATUS_ALREADY_SHUTDOWN. After shutdown,
 *     force_flush / set_as_global return OTEL_STATUS_ALREADY_SHUTDOWN and span creation
 *     becomes a no-op. A concurrent set_as_global and shutdown may linearize in either
 *     order: set_as_global may still publish the provider if it observes the SDK as
 *     not-yet-shut-down (which then becomes a no-op once shutdown completes); once
 *     shutdown is observed, set_as_global returns OTEL_STATUS_ALREADY_SHUTDOWN.
 *   - Metrics installation and Metrics shutdown are serialized per SDK. A concurrent
 *     otel_sdk_set_metrics_as_global() and otel_sdk_metrics_shutdown() linearize in lock
 *     acquisition order. If installation wins, shutdown removes that exact registration
 *     before shutting down the MeterProvider. If shutdown wins, installation returns
 *     OTEL_STATUS_ALREADY_SHUTDOWN and publishes nothing. Concurrent same-SDK installations
 *     are serialized; the last successful installation is the token later removed by
 *     shutdown or destroy.
 *   - Logs installation and Logs shutdown are serialized per SDK with the same rules, using
 *     their own lock and their own global slot: Logs lifecycle calls never affect the Trace
 *     or Metrics registrations, and vice versa.
 *   - A timed otel_sdk_force_flush() runs the flush on a helper thread; at most one such
 *     helper exists at a time (a concurrent timed flush returns OTEL_STATUS_TIMEOUT
 *     rather than spawning another). A blocking flush (timeout 0) uses the calling
 *     thread. See the function comment for details.
 *   - otel_sdk_destroy() must NOT race with any other call on the same handle; ensure all
 *     other SDK calls have returned (and, for a global install, that no spans are still
 *     being created) before destroying.
 *   - An otel_sdk_builder_t is NOT thread-safe; confine a builder to a single thread.
 *
 * Linking model
 * -------------
 * This header belongs to `libopentelemetry_c_sdk`. Applications link BOTH
 * `libopentelemetry_c_sdk` and `libopentelemetry_c_api` (and compile with both include
 * directories on the search path, since this header includes the API's common.h/trace.h).
 * Installing the SDK registers it into the API library's single global provider slot, so
 * instrumentation that links only `libopentelemetry_c_api` observes it.
 *
 * The shared global provider is guaranteed ONLY under dynamic linking with exactly one
 * loaded `libopentelemetry_c_api`. Statically linking the API into multiple artifacts
 * creates separate global slots and is NOT the shared-global model.
 *
 * Platform status: shared-library use is supported on Linux and macOS. Windows
 * shared-library use is unsupported because SDK-to-API import-library linkage is not
 * implemented.
 *
 * Library lifetime
 * ----------------
 * Once otel_sdk_set_as_global() succeeds, it publishes this library's static implementation
 * vtable and an SDK-owned provider object into the API global slot. otel_sdk_shutdown() and
 * otel_sdk_destroy() do NOT clear that slot (they stop/free the otel_sdk_t handle only); the
 * After either API or SDK library has been used, both must remain loaded until process exit.
 * Replacing providers, shutdown, and handle destruction do NOT make dlclose supported.
 *
 * Metrics unregistration or shutdown alone also does NOT make unloading the SDK safe.
 * Existing handles retain function pointers into `libopentelemetry_c_sdk`; destroy them for
 * normal lifecycle cleanup, but do not unload either library afterward.
 *
 * Using fork() without an immediate exec() after SDK background workers start is unsupported.
 *
 * Typical lifecycle
 * -----------------
 *   Build an exporter, wrap it in a span processor, then hand the processor to the SDK
 *   builder (see otlp_trace_exporter.h and batch_span_processor.h for the pipeline pieces):
 *
 *   otel_otlp_trace_exporter_builder_t* eb = otel_otlp_trace_exporter_builder_new();
 *   otel_otlp_trace_exporter_builder_set_endpoint(eb, otel_cstr("http://localhost:4318/v1/traces"));
 *   otel_trace_exporter_t* exporter = NULL;
 *   otel_otlp_trace_exporter_builder_build(eb, &exporter);
 *   otel_otlp_trace_exporter_builder_destroy(eb);
 *
 *   otel_batch_span_processor_builder_t* pb = otel_batch_span_processor_builder_new();
 *   otel_batch_span_processor_builder_set_exporter(pb, exporter); // ownership transfers on OK
 *   otel_span_processor_t* processor = NULL;
 *   otel_batch_span_processor_builder_build(pb, &processor);
 *   otel_batch_span_processor_builder_destroy(pb);
 *
 *   otel_sdk_builder_t* b = otel_sdk_builder_new();
 *   otel_sdk_builder_set_service_name(b, otel_cstr("my-service"));
 *   otel_sdk_builder_add_span_processor(b, processor); // ownership transfers on OK
 *   otel_sdk_t* sdk = NULL;
 *   if (otel_sdk_build(b, &sdk) == OTEL_STATUS_OK) {
 *       otel_sdk_set_as_global(sdk);
 *       ... create spans via the API ...
 *       otel_sdk_shutdown(sdk, 5000);
 *       otel_sdk_destroy(sdk);
 *   }
 *   otel_sdk_builder_destroy(b);
 */
#ifndef OPENTELEMETRY_C_SDK_H
#define OPENTELEMETRY_C_SDK_H

#include <opentelemetry_c/common.h>
#include <opentelemetry_c/metrics.h>
#include <opentelemetry_c/metric_view.h>
#include <opentelemetry_c/periodic_metric_reader.h>
#include <opentelemetry_c/manual_metric_reader.h>
#include <opentelemetry_c/trace.h>
#include <opentelemetry_c/span_processor.h>
#include <opentelemetry_c/logs.h>
#include <opentelemetry_c/log_processor.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Opaque handles. */
typedef struct otel_sdk_builder_t otel_sdk_builder_t;
typedef struct otel_sdk_t otel_sdk_t;

/* ---- Builder lifecycle ---------------------------------------------------- */

/* Create a new SDK builder with spec-default settings. NULL only on allocation
 * failure. Release with otel_sdk_builder_destroy(). */
otel_sdk_builder_t* otel_sdk_builder_new(void);

/* Destroy an SDK builder (no-op on NULL). Frees any span processors and Metrics readers
 * transferred to the builder but not yet consumed by otel_sdk_build(). */
void otel_sdk_builder_destroy(otel_sdk_builder_t* builder);

/* ---- Resource ------------------------------------------------------------- */

/* Set the `service.name` resource attribute. */
otel_status_t otel_sdk_builder_set_service_name(otel_sdk_builder_t* builder,
                                                otel_string_view_t name);

/* Add an arbitrary resource attribute. At most 1024 resource attributes may be added. */
otel_status_t otel_sdk_builder_add_resource_attribute(otel_sdk_builder_t* builder,
                                                      otel_key_value_t attribute);

/* ---- Sampler -------------------------------------------------------------- */

/* Built-in sampler kinds for otel_sampler_config_t::sampler_type. */
typedef enum otel_sampler_type_t {
  /* Sample every span. */
  OTEL_SAMPLER_ALWAYS_ON = 0,
  /* Drop every span. */
  OTEL_SAMPLER_ALWAYS_OFF = 1,
  /* Sample a deterministic fraction of traces based on the trace id (see `ratio`). */
  OTEL_SAMPLER_TRACE_ID_RATIO_BASED = 2,
  /* Respect the parent's sampled flag; for root spans, fall back to `parent_based_root_type`. */
  OTEL_SAMPLER_PARENT_BASED = 3
} otel_sampler_type_t;

/*
 * Versioned built-in sampler configuration. Set `struct_size` to sizeof(otel_sampler_config_t)
 * so the SDK reads only the fields your build knows about; the struct may grow in future
 * revisions without breaking existing callers.
 */
typedef struct otel_sampler_config_t {
  /* Size in bytes of this struct as compiled by the caller. Use OTEL_SAMPLER_CONFIG_INIT. */
  size_t struct_size;
  /* One of otel_sampler_type_t. */
  uint32_t sampler_type;
  /* Reserved; must be zero. */
  uint32_t reserved;
  /* Sampling probability in [0, 1]; used when the (root) sampler is ratio-based. */
  double ratio;
  /* For OTEL_SAMPLER_PARENT_BASED, the root sampler kind used when a span has no parent.
   * Must be a non-parent-based kind. Ignored for other sampler types. */
  uint32_t parent_based_root_type;
  /* Reserved; must be zero. */
  uint32_t reserved2;
} otel_sampler_config_t;

/* Zero-initializer that stamps struct_size and defaults to AlwaysOn. */
#define OTEL_SAMPLER_CONFIG_INIT \
  { sizeof(otel_sampler_config_t), OTEL_SAMPLER_ALWAYS_ON, 0u, 0.0, OTEL_SAMPLER_ALWAYS_ON, 0u }

#if defined(__STDC_VERSION__) && (__STDC_VERSION__ >= 201112L) && \
    defined(UINTPTR_MAX) && (UINTPTR_MAX == 0xFFFFFFFFFFFFFFFFu)
_Static_assert(sizeof(otel_sampler_config_t) == 32, "otel_sampler_config_t ABI mismatch");
#endif

/*
 * Configure the root sampler used by the tracer provider. Passing a NULL `config` clears any
 * override and restores the SDK default (ParentBased(AlwaysOn)). Calling this repeatedly
 * replaces the previously configured sampler.
 *
 * Validation: `struct_size` must be at least offsetof(parent_based_root_type); reserved fields
 * must be zero; `ratio` must be within [0, 1] for ratio-based (root) samplers; the parent-based
 * root type must itself be a non-parent-based kind.
 */
otel_status_t otel_sdk_builder_set_sampler(otel_sdk_builder_t* builder,
                                           const otel_sampler_config_t* config);

/* ---- Span limits ---------------------------------------------------------- */

/*
 * Versioned span-limit configuration. Set `struct_size` to sizeof(otel_span_limits_t) so the
 * SDK reads only the fields your build knows about; the struct may grow in future revisions
 * without breaking existing callers.
 *
 * Each bound caps how many attributes/events/links a span (or a single event/link) retains.
 * When a bound is exceeded, the most recently added items are dropped. A bound of 0 drops all
 * items in that collection.
 */
typedef struct otel_span_limits_t {
  /* Size in bytes of this struct as compiled by the caller. Use OTEL_SPAN_LIMITS_INIT. */
  size_t struct_size;
  /* Maximum attributes retained per span. */
  uint32_t max_attributes_per_span;
  /* Maximum events retained per span. */
  uint32_t max_events_per_span;
  /* Maximum links retained per span. */
  uint32_t max_links_per_span;
  /* Maximum attributes retained per event. */
  uint32_t max_attributes_per_event;
  /* Maximum attributes retained per link. */
  uint32_t max_attributes_per_link;
  /* Reserved; must be zero. */
  uint32_t reserved;
} otel_span_limits_t;

/* Initializer that stamps struct_size and sets every bound to the spec default of 128. */
#define OTEL_SPAN_LIMITS_INIT \
  { sizeof(otel_span_limits_t), 128u, 128u, 128u, 128u, 128u, 0u }

#if defined(__STDC_VERSION__) && (__STDC_VERSION__ >= 201112L) && \
    defined(UINTPTR_MAX) && (UINTPTR_MAX == 0xFFFFFFFFFFFFFFFFu)
_Static_assert(sizeof(otel_span_limits_t) == 32, "otel_span_limits_t ABI mismatch");
#endif

/*
 * Configure the span limits used by the tracer provider. Passing a NULL `config` clears any
 * override and restores the SDK defaults (128 for every bound). Calling this repeatedly
 * replaces the previously configured limits.
 *
 * Validation: `struct_size` must cover the full initial layout (through `reserved`) and the
 * `reserved` field must be zero.
 */
otel_status_t otel_sdk_builder_set_span_limits(otel_sdk_builder_t* builder,
                                               const otel_span_limits_t* config);

/* ---- Span processors ------------------------------------------------------ */

/*
 * Add (transfer) a span processor to the SDK's trace pipeline. Build the processor with a
 * span-processor builder (e.g. batch_span_processor.h), which in turn consumes a trace
 * exporter (e.g. otlp_trace_exporter.h).
 *
 * Ownership: on OTEL_STATUS_OK, ownership of `processor` transfers to the SDK builder, the
 * original pointer becomes invalid, and the caller must not access or destroy it. On failure
 * (invalid builder or processor), the caller still owns `processor`. Add more than one
 * processor to fan spans out to multiple pipelines. A builder accepts at most 64 span
 * processors. A builder with no span processor still builds a valid SDK whose spans are
 * simply not exported.
 */
otel_status_t otel_sdk_builder_add_span_processor(otel_sdk_builder_t* builder,
                                                  otel_span_processor_t* processor);

/* Add a periodic Metrics reader. On OTEL_STATUS_OK the reader is consumed and its original
 * pointer becomes invalid; on failure the caller still owns it. More than one reader may be
 * added; each maintains independent aggregation/temporality state. Periodic and manual
 * readers share a combined limit of 64 readers per SDK builder. */
otel_status_t otel_sdk_builder_add_metric_reader(otel_sdk_builder_t* builder,
                                                 otel_periodic_metric_reader_t* reader);
/* Add (transfer) a manual reader. A manual reader has no worker thread; after build,
 * otel_sdk_metrics_force_flush() performs its collection/export cycle. On OTEL_STATUS_OK the
 * reader is consumed and its original pointer becomes invalid; on failure the caller owns it. */
otel_status_t otel_sdk_builder_add_manual_metric_reader(
    otel_sdk_builder_t* builder, otel_manual_metric_reader_t* reader);
/* Add (transfer) a Metrics view. On OTEL_STATUS_OK the view is consumed and its original pointer
 * becomes invalid; on failure the caller still owns it. At most 1024 views may be added. */
otel_status_t otel_sdk_builder_add_metric_view(otel_sdk_builder_t* builder,
                                               otel_metric_view_t* view);

/* ---- Log processors (EXPERIMENTAL) ---------------------------------------- */

/*
 * Add (transfer) a log processor to the SDK's logs pipeline. Build the processor with
 * log_processor.h, which in turn consumes a log exporter (e.g. otlp_log_exporter.h).
 *
 * Ownership: on OTEL_STATUS_OK, ownership of `processor` transfers to the SDK builder and the
 * original pointer becomes invalid. On failure (invalid builder or processor, or the limit
 * being reached) the caller still owns `processor`. A builder accepts at most 64 log
 * processors. A builder with no log processor still builds a valid SDK whose log records are
 * simply not exported.
 */
otel_status_t otel_sdk_builder_add_log_processor(otel_sdk_builder_t* builder,
                                                 otel_log_processor_t* processor);

/* ---- Build ---------------------------------------------------------------- */

/*
 * Build an SDK from the accumulated configuration. On success writes a non-NULL handle
 * to *out_sdk and returns OTEL_STATUS_OK. On failure returns an error status, sets
 * *out_sdk to NULL, and records a message retrievable via otel_last_error_message().
 *
 * The span processors transferred to the builder move into the built SDK; the builder
 * remains owned by the caller and must still be destroyed. Note that a second build on the
 * same builder produces an SDK with no processors (they were consumed by the first build).
 */
otel_status_t otel_sdk_build(otel_sdk_builder_t* builder, otel_sdk_t** out_sdk);

/* ---- Provider access and global installation ------------------------------ */

/*
 * Return an owned tracer-provider handle backed by this SDK. Independent of the SDK
 * handle's lifetime; release with otel_tracer_provider_destroy(). NULL if `sdk` is
 * invalid.
 */
otel_tracer_provider_t* otel_sdk_get_tracer_provider(const otel_sdk_t* sdk);
otel_meter_provider_t* otel_sdk_get_meter_provider(const otel_sdk_t* sdk);

/*
 * Install this SDK's provider as the process-global provider. May be called more than
 * once; the most recent call wins. Returns OTEL_STATUS_ALREADY_SHUTDOWN if the SDK has
 * been shut down.
 *
 * A concurrent set_as_global and otel_sdk_shutdown() may linearize in either order: if
 * set_as_global observes the SDK as not-yet-shut-down it may publish the provider (which
 * then becomes a no-op once shutdown completes); once shutdown is observed, set_as_global
 * returns OTEL_STATUS_ALREADY_SHUTDOWN.
 */
otel_status_t otel_sdk_set_as_global(otel_sdk_t* sdk);

/*
 * Install this SDK's MeterProvider as the process-global Metrics provider. Repeated and
 * concurrent calls on one SDK are serialized; the most recent successful call wins.
 * Concurrent Metrics shutdown either follows a completed installation and removes it, or
 * precedes installation and causes OTEL_STATUS_ALREADY_SHUTDOWN. An older SDK's shutdown
 * never removes a provider registered later by another SDK.
 */
otel_status_t otel_sdk_set_metrics_as_global(otel_sdk_t* sdk);

/*
 * EXPERIMENTAL. Return an owned logger-provider handle backed by this SDK. Independent of the
 * SDK handle's lifetime; release with otel_logger_provider_destroy(). NULL if `sdk` is
 * invalid.
 */
otel_logger_provider_t* otel_sdk_get_logger_provider(const otel_sdk_t* sdk);

/*
 * EXPERIMENTAL. Install this SDK's LoggerProvider as the process-global Logs provider. The
 * Logs global slot is independent of the Trace and Metrics slots: installing here neither
 * replaces nor is replaced by the other signals. Repeated and concurrent calls on one SDK are
 * serialized; the most recent successful call wins. Returns OTEL_STATUS_ALREADY_SHUTDOWN once
 * Logs shutdown has been observed.
 */
otel_status_t otel_sdk_set_logs_as_global(otel_sdk_t* sdk);

/* ---- Lifecycle ------------------------------------------------------------ */

/*
 * Flush buffered spans.
 *   - timeout_millis == 0: block on the calling thread until the flush completes.
 *   - timeout_millis  > 0: run the flush on a helper thread and return
 *     OTEL_STATUS_TIMEOUT if it does not finish in time (the flush continues in the
 *     background). At most one timed-flush helper thread runs at a time: while one is in
 *     flight, a concurrent timed flush returns OTEL_STATUS_TIMEOUT immediately instead of
 *     spawning another thread. Returns OTEL_STATUS_INTERNAL_ERROR if the helper thread
 *     cannot be spawned, or OTEL_STATUS_ALREADY_SHUTDOWN after shutdown.
 */
otel_status_t otel_sdk_force_flush(otel_sdk_t* sdk, uint64_t timeout_millis);

/*
 * Shut down the SDK, flushing and stopping the pipeline. The underlying shutdown runs at
 * most once: the first call performs it and returns its result; concurrent or subsequent
 * calls return OTEL_STATUS_ALREADY_SHUTDOWN without side effects. `timeout_millis` of 0
 * uses the SDK default (5s). After shutdown, span creation through this SDK becomes a
 * no-op. This should be called before process exit to avoid losing buffered spans.
 */
otel_status_t otel_sdk_shutdown(otel_sdk_t* sdk, uint64_t timeout_millis);

/*
 * Force every Metrics reader to collect and export. For a manual reader this is the sole
 * application-controlled collection trigger and runs synchronously on the calling thread.
 * The pinned Rust 0.32 reader API does not accept timeout_millis, so this call currently
 * ignores that argument and can block indefinitely if an exporter or collection callback
 * does not return. Unlike the trace provider, Metrics may own an async runtime outside the
 * cloneable provider. A detached timeout helper could therefore outlive that runtime after
 * SDK destruction; bounded Metrics flush requires coordinated runtime and shutdown ownership.
 */
otel_status_t otel_sdk_metrics_force_flush(otel_sdk_t* sdk, uint64_t timeout_millis);

/* Metrics shutdown is independent from trace shutdown and runs at most once. If this SDK
 * still owns the API global Metrics slot, shutdown removes that registration before shutting
 * down the provider; a newer SDK registration is never cleared by an older SDK. The pinned
 * Rust provider ignores timeout_millis; PeriodicReader uses its own fixed five-second wait. */
otel_status_t otel_sdk_metrics_shutdown(otel_sdk_t* sdk, uint64_t timeout_millis);

/*
 * EXPERIMENTAL. Flush every configured log processor.
 *
 * Unlike the Trace and Metrics equivalents this takes NO caller timeout, because the pinned
 * upstream LoggerProvider force-flush accepts none. Its synchronous batch processor applies
 * an internal, non-configurable five-second wait; if that wait expires this function returns
 * a non-OK export-pipeline status while the worker may still be exporting. Configurable
 * support can be added later through a new function such as
 * otel_sdk_logs_force_flush_with_timeout(), leaving this C signature unchanged. Returns
 * OTEL_STATUS_ALREADY_SHUTDOWN after Logs shutdown.
 */
otel_status_t otel_sdk_logs_force_flush(otel_sdk_t* sdk);

/*
 * EXPERIMENTAL. Shut down the LoggerProvider. Independent of trace and Metrics shutdown, and
 * runs at most once: the first call performs it; concurrent or later calls return
 * OTEL_STATUS_ALREADY_SHUTDOWN. If this SDK still owns the API global Logs slot, that
 * registration is removed BEFORE the provider is shut down, so no C caller can obtain a
 * logger from a provider that is about to stop accepting records. A newer SDK's registration
 * is never cleared by an older SDK. `timeout_millis` of 0 uses the SDK default (5s). After
 * shutdown, emitting through this SDK's loggers becomes a no-op.
 */
otel_status_t otel_sdk_logs_shutdown(otel_sdk_t* sdk, uint64_t timeout_millis);

/*
 * Destroy an SDK handle (no-op on NULL). If not already shut down, dropping the SDK
 * triggers a best-effort shutdown; prefer calling the signal-specific shutdown functions
 * explicitly. Destroy also conditionally removes this SDK's global Metrics and Logs
 * registrations.
 * Must not race with any other call on the same SDK handle.
 */
void otel_sdk_destroy(otel_sdk_t* sdk);

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* OPENTELEMETRY_C_SDK_H */
