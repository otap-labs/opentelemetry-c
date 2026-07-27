# Fuzzing

The `fuzz/` package uses `cargo-fuzz` and libFuzzer with structured, valid-memory inputs.
Targets never synthesize arbitrary addresses or reuse freed handles.

| Target | Coverage |
| --- | --- |
| `metrics_inputs` | UTF-8, NULL/length pairs, attribute tags and values, option prefixes, duplicate scope keys, histogram boundaries, counts, and scalar recording. |
| `handle_kinds` | Live wrong-type opaque handles and bounded operation sequences without double destroy or dangling-pointer access. |
| `exporter_visitor` | Custom exporter callback-table size, temporality and callback presence; visitor size; callback status propagation; manual collection; and exactly-once state destruction. |
| `logs_records` | Log record prefix sizes, presence bits, reserved words, severity numbers, value tags, trace context, and — most importantly — the flat value node pool: child ranges that are out of bounds, backwards, self-referential, shared between parents, or unreferenced. |

`handle_kinds` also covers cross-signal confusion between Logs and Metrics handles, which is
worth calling out because loggers and meters are resolved from entirely separate global slots
and are distinguished only by the kind tag stored in the handle.

The Logs target deserves a note on how it stays honest. It never hands the implementation an
address invented by the fuzzer; only lengths, tags, indices, and structure sizes are fuzzed, and
every pointer is either NULL or points at a live Rust buffer that outlives the call. NULL paired
with a non-zero length *is* generated deliberately, because rejecting that pair before any
dereference is precisely the property under test rather than a bug the fuzzer would be papering
over.

Install the pinned tool and run every target:

```sh
cargo install cargo-fuzz --version 0.13.2 --locked
METRICS_FUZZ_SECONDS=10 scripts/fuzz-metrics.sh
LOGS_FUZZ_SECONDS=10 scripts/fuzz-logs.sh
```

For a longer VM session, set `METRICS_FUZZ_LONG_SECONDS` or `LOGS_FUZZ_LONG_SECONDS`; the
highest-risk targets receive the additional run. Inputs are capped at 4096 bytes and RSS at 2 GiB.
Preserve only minimized, reviewable regression inputs under version control.
