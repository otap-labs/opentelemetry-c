# Metrics fuzzing

The `fuzz/` package uses `cargo-fuzz` and libFuzzer with structured, valid-memory inputs.
Targets never synthesize arbitrary addresses or reuse freed handles.

| Target | Coverage |
| --- | --- |
| `metrics_inputs` | UTF-8, NULL/length pairs, attribute tags and values, option prefixes, duplicate scope keys, histogram boundaries, counts, and scalar recording. |
| `handle_kinds` | Live wrong-type opaque handles and bounded operation sequences without double destroy or dangling-pointer access. |
| `exporter_visitor` | Custom exporter callback-table size, temporality and callback presence; visitor size; callback status propagation; manual collection; and exactly-once state destruction. |

Install the pinned tool and run every target:

```sh
cargo install cargo-fuzz --version 0.13.2 --locked
METRICS_FUZZ_SECONDS=10 scripts/fuzz-metrics.sh
```

For a longer VM session, set `METRICS_FUZZ_LONG_SECONDS`; the highest-risk input and visitor
targets receive the additional run. Inputs are capped at 4096 bytes and RSS at 2 GiB.
Preserve only minimized, reviewable regression inputs under version control.
