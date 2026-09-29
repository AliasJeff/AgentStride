# Deterministic Evaluation

`ag-eval` measures AgentStride's execution control plane with deterministic
fake workers. It does not measure model intelligence and requires no API key.

## Run

```bash
cargo run --bin ag-eval -- --scenario chain --agents 2 --seed 7
cargo run --bin ag-eval -- --scenario diamond --agents 4 --seed 7
cargo run --bin ag-eval -- --scenario wide --agents 4 --seed 7

# Durable Runtime variant
cargo run --bin ag-eval -- --variant runtime --scenario chain --agents 2 --seed 7
```

The output is JSON and records:

```text
completed
dependency_violations
duplicate_claims
journal_events
success
```

The graph-controlled fake worker is the baseline. Add a direct-execution
variant only when it uses the same workload, seed, and termination rules.
Keep raw JSON results under a local `eval/results/` directory; generated
results are not source files.

The `runtime` variant exercises Run History, reconciliation, durable task
polling, signed worker tokens, timeout timers, retries, and history
verification. Its output also includes `plan_revision`, `history_version`,
and `recovered`.

The minimum correctness target is:

```text
success = true
dependency_violations = 0
duplicate_claims = 0
```
