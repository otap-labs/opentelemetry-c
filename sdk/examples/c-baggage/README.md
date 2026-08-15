<!-- SPDX-License-Identifier: Apache-2.0 -->

# C baggage across instrumentation libraries

This API-only example extracts a W3C `baggage` header at a request edge, attaches the resulting
immutable context, and reads `tenant.id` from a separate C translation unit. It demonstrates
the intended cross-library model without installing or linking an SDK.

```sh
make run
```

Expected output: `tenant.id=acme`.

Do not put secrets or personal data in baggage, and clear it before sending a request across an
untrusted boundary. Baggage is propagated data, not telemetry attributes.
