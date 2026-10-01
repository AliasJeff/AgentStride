# Development

## Repository Layout

```text
src/domain/     typed values and invariants
src/graph/      cycle checks and ready-frontier logic
src/store/      SQLite schema, transactions, journal, runtime persistence
src/plan/       Git plan export and reconciliation
src/runtime/    commands, events, reducer, workers, provider
src/runner/     capability tokens and command policy
src/agent/      prime, memory, hooks, setup
src/web/        read-only observability API and UI server
tests/          CLI, integration, concurrency, replay, and fault tests
```

## Local Workflow

```bash
cargo build --bin ag
cargo test --all-targets
```

Initialize a workspace:

```bash
ag init
```

Use the task graph:

```bash
ag add "Implement a durable store"
ag ready --json
ag claim-next --actor agent-a --json
ag checkpoint <id> --actor agent-a \
  --summary "Store boundary implemented" \
  --next-step "Add recovery test"
ag close <id> --reason "Tests pass"
```

Run the durable runtime demo:

```bash
./scripts/durable-runtime-demo.sh
```

## Quality Gates

Before a code change is considered complete:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets
```

The project also includes:

- Criterion benchmarks in `benches/engine.rs`;
- deterministic fake-worker evaluation in `src/bin/ag-eval.rs`;
- process-level crash and lease recovery tests;
- JSONL export, import, replay, and verification tests.

## Design Rules

- Keep domain rules below the CLI.
- Keep SQL and transaction ownership in store modules.
- Validate before writing.
- Commit state and its journal event together.
- Derive readiness from authoritative state.
- Treat revisions as equality guards, not ordering values.
- Keep JSON output stable and machine-readable.
- Prefer the smallest abstraction that makes an invariant obvious.

## Non-Goals

This repository intentionally does not include a distributed server, remote
database, general workflow DSL, multi-tenant authorization, or a full sandbox.
Record future hardening work in the architecture or release notes rather than
expanding the core opportunistically.
