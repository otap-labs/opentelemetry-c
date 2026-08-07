#include <stdio.h>
#include <opentelemetry_c/api.h>

void worker_read_current_baggage(void) {
    otel_context_t* current = otel_context_current();
    otel_baggage_t* baggage = otel_context_baggage(current);
    otel_baggage_entry_view_t entry = OTEL_BAGGAGE_ENTRY_VIEW_INIT;
    if (baggage != NULL &&
        otel_baggage_get(baggage, otel_cstr("tenant.id"), &entry) == OTEL_TRUE) {
        printf("tenant.id=%.*s\n", (int)entry.value.len, entry.value.ptr);
    }
    otel_baggage_destroy(baggage);
    otel_context_destroy(current);
}
