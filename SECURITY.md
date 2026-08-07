<!-- SPDX-License-Identifier: Apache-2.0 -->

# Security policy

`opentelemetry-c` is experimental. Until multiple release lines are maintained, only the
latest tagged release receives security fixes.

Report suspected vulnerabilities through
[GitHub private vulnerability reporting](https://github.com/otap-labs/opentelemetry-c/security/advisories/new).
Do not disclose suspected vulnerabilities in public issues.

## Baggage data

W3C Baggage is propagated to downstream services and may cross trust boundaries. Do not place
credentials, secrets, or personal information in baggage. Applications should inspect or clear
baggage before sending requests outside their trusted environment. Baggage is never copied into
telemetry attributes automatically, and diagnostics do not include baggage values.

A report should include the affected version and platform, impact, reproduction steps or a
proof of concept, relevant configuration, and any suggested mitigation. Maintainers will
coordinate disclosure based on severity and fix availability; no fixed response SLA is
promised during the experimental stage.

Supported versions and reporting procedures may change as the project matures.
