// SPDX-License-Identifier: Apache-2.0

#include <opentelemetry_c/api.h>

void worker_read_current_baggage(void);

int main(void) {
    const char header[] = "tenant.id=acme,region=us-west";
    otel_baggage_t* baggage = NULL;
    otel_context_t* empty = NULL;
    otel_context_t* request = NULL;
    otel_context_scope_t scope = OTEL_CONTEXT_SCOPE_INIT;
    otel_string_view_t wire = {header, sizeof(header) - 1};

    if (otel_baggage_propagation_extract(wire, &baggage) != OTEL_STATUS_OK) return 1;
    empty = otel_context_create(NULL);
    if (empty == NULL ||
        otel_context_with_baggage(empty, baggage, &request) != OTEL_STATUS_OK ||
        otel_context_attach(request, &scope) != OTEL_STATUS_OK) {
        otel_context_destroy(request);
        otel_context_destroy(empty);
        otel_baggage_destroy(baggage);
        return 1;
    }
    worker_read_current_baggage();
    (void)otel_context_scope_detach(&scope);
    otel_context_destroy(request);
    otel_context_destroy(empty);
    otel_baggage_destroy(baggage);
    return 0;
}
