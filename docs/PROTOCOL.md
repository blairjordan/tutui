# External scenario protocol

The `process` scenario runs any command and reads **one JSON object per line** from
its stdout. Anything that is not valid JSON is shown as an info log line. The
child receives its parameters as JSON in the `TUTUI_PARAMS` environment
variable and may receive `{"type":"stop"}` on stdin when the user stops the run;
it then has `stop_grace_seconds` (default 5) to exit before being killed.

## Events (child → dashboard)

| type      | fields | meaning |
|-----------|--------|---------|
| `hello`   | `scenario`, `metrics: [{name, kind, unit?, description?}]` | declare metrics before reporting them. `kind` ∈ `counter` · `gauge` · `histogram` |
| `phase`   | `name` | start a named phase; the report tabulates every metric per phase |
| `observe` | `metric`, `value`, `labels?: {k: v}` | one observation |
| `batch`   | `observations: [observe…]` | many observations in one line (preferred on hot paths) |
| `log`     | `level` ∈ `debug` `info` `warn` `error`, `message` | shown in the log pane |
| `done`    | `summary?` | scenario finished; `summary` is copied into the report |
| `error`   | `message` | scenario failed |

Counters are graphed as a per-second rate, gauges as their last value,
histograms as p50/p95/p99 per second with overall percentiles in the report.
Labels create a breakdown row per distinct label set (keep cardinality low).

Metrics that were never declared are counted in `unknown_metrics` and otherwise ignored.

## Minimal example

```
{"type":"hello","scenario":"demo","metrics":[{"name":"requests","kind":"counter"},{"name":"latency_ms","kind":"histogram","unit":"ms"}]}
{"type":"phase","name":"warm"}
{"type":"batch","observations":[{"metric":"requests","value":1,"labels":{"status":"200"}},{"metric":"latency_ms","value":42.1}]}
{"type":"done","summary":{"ok":1}}
```
