// SPDX-License-Identifier: Apache-2.0

/*
 * opentelemetry_c/metrics.h
 *
 * Experimental OpenTelemetry Metrics API. Meter providers, meters, and instruments are
 * concurrency-safe. Destroy must not race with another operation on the same handle.
 *
 * Without an installed Metrics SDK, valid instrument creation succeeds and measurements
 * are allocation-free no-ops. Instrument configuration is still validated consistently.
 * A Meter acquired while no Metrics SDK is installed remains no-op; installing an SDK later
 * does not retrofit existing Meter or instrument handles. Reacquire the Meter and recreate
 * its instruments after installing the global SDK. This is expected handle behavior.
 */
#ifndef OPENTELEMETRY_C_METRICS_H
#define OPENTELEMETRY_C_METRICS_H

#include "common.h"

#ifdef __cplusplus
extern "C" {
#endif

typedef struct otel_meter_provider_t otel_meter_provider_t;
typedef struct otel_meter_t otel_meter_t;
typedef struct otel_counter_u64_t otel_counter_u64_t;
typedef struct otel_counter_f64_t otel_counter_f64_t;
typedef struct otel_up_down_counter_i64_t otel_up_down_counter_i64_t;
typedef struct otel_up_down_counter_f64_t otel_up_down_counter_f64_t;
typedef struct otel_gauge_u64_t otel_gauge_u64_t;
typedef struct otel_gauge_i64_t otel_gauge_i64_t;
typedef struct otel_gauge_f64_t otel_gauge_f64_t;
typedef struct otel_histogram_u64_t otel_histogram_u64_t;
typedef struct otel_histogram_f64_t otel_histogram_f64_t;
typedef struct otel_bound_counter_u64_t otel_bound_counter_u64_t;
typedef struct otel_bound_counter_f64_t otel_bound_counter_f64_t;
typedef struct otel_bound_histogram_u64_t otel_bound_histogram_u64_t;
typedef struct otel_bound_histogram_f64_t otel_bound_histogram_f64_t;
typedef struct otel_observable_counter_u64_t otel_observable_counter_u64_t;
typedef struct otel_observable_counter_f64_t otel_observable_counter_f64_t;
typedef struct otel_observable_up_down_counter_i64_t otel_observable_up_down_counter_i64_t;
typedef struct otel_observable_up_down_counter_f64_t otel_observable_up_down_counter_f64_t;
typedef struct otel_observable_gauge_u64_t otel_observable_gauge_u64_t;
typedef struct otel_observable_gauge_i64_t otel_observable_gauge_i64_t;
typedef struct otel_observable_gauge_f64_t otel_observable_gauge_f64_t;
typedef struct otel_observer_u64_t otel_observer_u64_t;
typedef struct otel_observer_i64_t otel_observer_i64_t;
typedef struct otel_observer_f64_t otel_observer_f64_t;

typedef void (*otel_observable_callback_u64_t)(otel_observer_u64_t* observer, void* user_data);
typedef void (*otel_observable_callback_i64_t)(otel_observer_i64_t* observer, void* user_data);
typedef void (*otel_observable_callback_f64_t)(otel_observer_f64_t* observer, void* user_data);
typedef void (*otel_user_data_destroy_t)(void* user_data);

/*
 * Extensible creation options. Initialize with OTEL_INSTRUMENT_OPTIONS_INIT.
 * Description is arbitrary UTF-8. Unit is case-sensitive ASCII, at most 63 bytes.
 * Histogram boundaries are borrowed for the creation call and must be finite and strictly
 * increasing. For non-histograms boundaries must be NULL and boundary_count zero.
 */
typedef struct otel_instrument_options_t {
    uint64_t struct_size;
    otel_string_view_t description;
    otel_string_view_t unit;
    const double* boundaries;
    size_t boundary_count;
} otel_instrument_options_t;

/*
 * Complete instrumentation-scope options. Initialize with OTEL_METER_OPTIONS_INIT.
 * All strings and attributes are borrowed only for the call and copied by an SDK-backed
 * provider. Scope attribute keys must be non-empty, valid UTF-8, and unique. Duplicate keys
 * are rejected rather than assigned ordering-dependent semantics.
 */
typedef struct otel_meter_options_t {
    uint64_t struct_size;
    otel_string_view_t name;
    otel_string_view_t version;
    otel_string_view_t schema_url;
    const otel_key_value_t* attributes;
    size_t attribute_count;
} otel_meter_options_t;

#define OTEL_INSTRUMENT_OPTIONS_INIT \
    { sizeof(otel_instrument_options_t), { NULL, 0 }, { NULL, 0 }, NULL, 0 }

#define OTEL_METER_OPTIONS_INIT \
    { sizeof(otel_meter_options_t), { NULL, 0 }, { NULL, 0 }, { NULL, 0 }, NULL, 0 }

#if defined(__STDC_VERSION__) && (__STDC_VERSION__ >= 201112L) && \
    defined(UINTPTR_MAX) && (UINTPTR_MAX == 0xFFFFFFFFFFFFFFFFu)
_Static_assert(sizeof(otel_instrument_options_t) == 56,
               "otel_instrument_options_t ABI mismatch");
_Static_assert(sizeof(otel_meter_options_t) == 72,
               "otel_meter_options_t ABI mismatch");
#endif

otel_meter_provider_t* otel_global_meter_provider(void);
otel_meter_t* otel_meter_provider_get_meter(const otel_meter_provider_t* provider,
                                            otel_string_view_t name,
                                            otel_string_view_t version,
                                            otel_string_view_t schema_url);
otel_meter_t* otel_meter_provider_get_meter_with_options(
    const otel_meter_provider_t* provider, const otel_meter_options_t* options);
void otel_meter_provider_destroy(otel_meter_provider_t* provider);
void otel_meter_destroy(otel_meter_t* meter);

#define OTEL_DECLARE_METRIC_CREATE(fn, type) \
    otel_status_t fn(const otel_meter_t* meter, otel_string_view_t name, \
                     const otel_instrument_options_t* options, type** out)

OTEL_DECLARE_METRIC_CREATE(otel_meter_create_u64_counter, otel_counter_u64_t);
OTEL_DECLARE_METRIC_CREATE(otel_meter_create_f64_counter, otel_counter_f64_t);
OTEL_DECLARE_METRIC_CREATE(otel_meter_create_i64_up_down_counter, otel_up_down_counter_i64_t);
OTEL_DECLARE_METRIC_CREATE(otel_meter_create_f64_up_down_counter, otel_up_down_counter_f64_t);
OTEL_DECLARE_METRIC_CREATE(otel_meter_create_u64_gauge, otel_gauge_u64_t);
OTEL_DECLARE_METRIC_CREATE(otel_meter_create_i64_gauge, otel_gauge_i64_t);
OTEL_DECLARE_METRIC_CREATE(otel_meter_create_f64_gauge, otel_gauge_f64_t);
OTEL_DECLARE_METRIC_CREATE(otel_meter_create_u64_histogram, otel_histogram_u64_t);
OTEL_DECLARE_METRIC_CREATE(otel_meter_create_f64_histogram, otel_histogram_f64_t);

#undef OTEL_DECLARE_METRIC_CREATE

#define OTEL_DECLARE_METRIC_OPERATION(fn, type, value_type) \
    otel_status_t fn(const type* instrument, value_type value, \
                     const otel_key_value_t* attributes, size_t attribute_count)

OTEL_DECLARE_METRIC_OPERATION(otel_counter_u64_add, otel_counter_u64_t, uint64_t);
OTEL_DECLARE_METRIC_OPERATION(otel_counter_f64_add, otel_counter_f64_t, double);
OTEL_DECLARE_METRIC_OPERATION(otel_up_down_counter_i64_add, otel_up_down_counter_i64_t, int64_t);
OTEL_DECLARE_METRIC_OPERATION(otel_up_down_counter_f64_add, otel_up_down_counter_f64_t, double);
OTEL_DECLARE_METRIC_OPERATION(otel_gauge_u64_record, otel_gauge_u64_t, uint64_t);
OTEL_DECLARE_METRIC_OPERATION(otel_gauge_i64_record, otel_gauge_i64_t, int64_t);
OTEL_DECLARE_METRIC_OPERATION(otel_gauge_f64_record, otel_gauge_f64_t, double);
OTEL_DECLARE_METRIC_OPERATION(otel_histogram_u64_record, otel_histogram_u64_t, uint64_t);
OTEL_DECLARE_METRIC_OPERATION(otel_histogram_f64_record, otel_histogram_f64_t, double);

#undef OTEL_DECLARE_METRIC_OPERATION

void otel_counter_u64_destroy(otel_counter_u64_t* instrument);
void otel_counter_f64_destroy(otel_counter_f64_t* instrument);
void otel_up_down_counter_i64_destroy(otel_up_down_counter_i64_t* instrument);
void otel_up_down_counter_f64_destroy(otel_up_down_counter_f64_t* instrument);
void otel_gauge_u64_destroy(otel_gauge_u64_t* instrument);
void otel_gauge_i64_destroy(otel_gauge_i64_t* instrument);
void otel_gauge_f64_destroy(otel_gauge_f64_t* instrument);
void otel_histogram_u64_destroy(otel_histogram_u64_t* instrument);
void otel_histogram_f64_destroy(otel_histogram_f64_t* instrument);

/*
 * Experimental bound instruments pre-convert and bind an attribute set once, so repeated
 * recordings do not cross the C ABI with attributes and do not allocate for attribute
 * conversion. The bound handle owns the copied attributes and remains valid independently
 * of the source instrument handle. Without an installed SDK, binding succeeds and produces
 * a no-op bound handle without reading the attributes.
 * Binding against an SDK vtable that predates this extension returns
 * OTEL_STATUS_INVALID_CONFIG. Bound handles may be recorded concurrently from multiple
 * threads. Destruction must not race with recording on the same handle.
 *
 * If the SDK stream's cardinality limit is already exhausted when binding, the handle is
 * permanently attached to the overflow series. Drop and re-bind after capacity becomes
 * available to resolve the original attribute set again.
 *
 * The pinned opentelemetry-rust 0.32 API supports bound counters and histograms only;
 * gauges and up/down counters therefore have no bound C API.
 */
otel_status_t otel_counter_u64_bind(const otel_counter_u64_t* instrument,
                                    const otel_key_value_t* attributes,
                                    size_t attribute_count,
                                    otel_bound_counter_u64_t** out);
otel_status_t otel_counter_f64_bind(const otel_counter_f64_t* instrument,
                                    const otel_key_value_t* attributes,
                                    size_t attribute_count,
                                    otel_bound_counter_f64_t** out);
otel_status_t otel_histogram_u64_bind(const otel_histogram_u64_t* instrument,
                                      const otel_key_value_t* attributes,
                                      size_t attribute_count,
                                      otel_bound_histogram_u64_t** out);
otel_status_t otel_histogram_f64_bind(const otel_histogram_f64_t* instrument,
                                      const otel_key_value_t* attributes,
                                      size_t attribute_count,
                                      otel_bound_histogram_f64_t** out);

otel_status_t otel_bound_counter_u64_add(const otel_bound_counter_u64_t* instrument,
                                         uint64_t value);
otel_status_t otel_bound_counter_f64_add(const otel_bound_counter_f64_t* instrument,
                                         double value);
otel_status_t otel_bound_histogram_u64_record(
    const otel_bound_histogram_u64_t* instrument, uint64_t value);
otel_status_t otel_bound_histogram_f64_record(
    const otel_bound_histogram_f64_t* instrument, double value);

void otel_bound_counter_u64_destroy(otel_bound_counter_u64_t* instrument);
void otel_bound_counter_f64_destroy(otel_bound_counter_f64_t* instrument);
void otel_bound_histogram_u64_destroy(otel_bound_histogram_u64_t* instrument);
void otel_bound_histogram_f64_destroy(otel_bound_histogram_f64_t* instrument);

/*
 * Observable callbacks run during reader collection and may be invoked concurrently by
 * different readers. An observer token is valid only on the callback's current thread and
 * only until that callback returns. It may be used repeatedly and reentrantly on that thread,
 * but must not be retained, passed to another thread, or used by work spawned from the
 * callback. Cross-thread and stale uses return OTEL_STATUS_INVALID_ARGUMENT without
 * dispatching to the SDK. Callbacks must finish in finite time.
 * On successful observable creation, the instrument owns user_data and invokes
 * user_data_destroy exactly once. On failure, ownership remains with the caller and the
 * library does not invoke user_data_destroy.
 * Destroying the public observable handle disables future callback work and releases its
 * user-data ownership. user_data_destroy runs exactly once after any callback already in
 * flight releases its temporary reference; it does not depend on the SDK pipeline closure
 * being unregistered or dropped. opentelemetry-rust 0.32 does not support callback
 * unregistration, so the internal no-op pipeline callback registration may remain until its
 * MeterProvider shuts down or is dropped. Repeatedly creating and destroying observables can
 * therefore accumulate inactive registrations retained for the provider lifetime; this is
 * provider-lifetime retention, not a permanent memory leak. The library contains panics from
 * its own Rust trampoline logic before they can unwind across the ABI. This does not make
 * unwinding or exceptions from a user callback across its C function boundary safe.
 */
#define OTEL_DECLARE_OBSERVABLE_CREATE(fn, type, callback_type) \
    otel_status_t fn(const otel_meter_t* meter, otel_string_view_t name, \
                     const otel_instrument_options_t* options, callback_type callback, \
                     void* user_data, otel_user_data_destroy_t user_data_destroy, type** out)

OTEL_DECLARE_OBSERVABLE_CREATE(otel_meter_create_u64_observable_counter,
                               otel_observable_counter_u64_t,
                               otel_observable_callback_u64_t);
OTEL_DECLARE_OBSERVABLE_CREATE(otel_meter_create_f64_observable_counter,
                               otel_observable_counter_f64_t,
                               otel_observable_callback_f64_t);
OTEL_DECLARE_OBSERVABLE_CREATE(otel_meter_create_i64_observable_up_down_counter,
                               otel_observable_up_down_counter_i64_t,
                               otel_observable_callback_i64_t);
OTEL_DECLARE_OBSERVABLE_CREATE(otel_meter_create_f64_observable_up_down_counter,
                               otel_observable_up_down_counter_f64_t,
                               otel_observable_callback_f64_t);
OTEL_DECLARE_OBSERVABLE_CREATE(otel_meter_create_u64_observable_gauge,
                               otel_observable_gauge_u64_t,
                               otel_observable_callback_u64_t);
OTEL_DECLARE_OBSERVABLE_CREATE(otel_meter_create_i64_observable_gauge,
                               otel_observable_gauge_i64_t,
                               otel_observable_callback_i64_t);
OTEL_DECLARE_OBSERVABLE_CREATE(otel_meter_create_f64_observable_gauge,
                               otel_observable_gauge_f64_t,
                               otel_observable_callback_f64_t);

#undef OTEL_DECLARE_OBSERVABLE_CREATE

otel_status_t otel_observer_u64_observe(otel_observer_u64_t* observer, uint64_t value,
                                        const otel_key_value_t* attributes,
                                        size_t attribute_count);
otel_status_t otel_observer_i64_observe(otel_observer_i64_t* observer, int64_t value,
                                        const otel_key_value_t* attributes,
                                        size_t attribute_count);
otel_status_t otel_observer_f64_observe(otel_observer_f64_t* observer, double value,
                                        const otel_key_value_t* attributes,
                                        size_t attribute_count);

void otel_observable_counter_u64_destroy(otel_observable_counter_u64_t* instrument);
void otel_observable_counter_f64_destroy(otel_observable_counter_f64_t* instrument);
void otel_observable_up_down_counter_i64_destroy(
    otel_observable_up_down_counter_i64_t* instrument);
void otel_observable_up_down_counter_f64_destroy(
    otel_observable_up_down_counter_f64_t* instrument);
void otel_observable_gauge_u64_destroy(otel_observable_gauge_u64_t* instrument);
void otel_observable_gauge_i64_destroy(otel_observable_gauge_i64_t* instrument);
void otel_observable_gauge_f64_destroy(otel_observable_gauge_f64_t* instrument);

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* OPENTELEMETRY_C_METRICS_H */
