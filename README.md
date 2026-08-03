# AgentStride

> Durable execution for long-running Agent work.

AgentStride is a local-first Rust runtime for coding tasks that must survive
context loss, worker failure, retries, process restarts, and plan changes.

It keeps two kinds of durable facts separate:

```text
Plan layer       Git-versioned tasks and dependencies
Execution layer  SQLite history, leases, timers, and outcomes
```

The project is intentionally small: one binary, one SQLite database, and no
server dependency.

## Why

Long-running Agent work is not a normal task queue:

- the plan can change while execution is in progress;
- an Agent can lose context or stop halfway through a task;
- workers can crash after claiming work;
- retries must not allow stale workers to overwrite newer attempts;
- recovery must be explainable from durable history.

AgentStride treats the plan as versioned data and execution as an append-only
state machine. A reconciler joins the two without rewriting completed history.

## Core Model

```text
Git PlanRevision
        |
        v
Run -> Durable Task -> Worker Attempt
 |                         |
 +-> History <-------------+
        |
        +-> checkpoint / retry / timeout / recovery / handoff
```

Key guarantees:

- scheduling edges remain acyclic;
- ready work is derived from authoritative state;
- claims and leases are transactional;
- stale attempts are fenced by signed tokens;
- state and events commit together;
- history can rebuild and verify materialized runtime state;
- plan changes require an approved revision;
- replay is explicit and idempotent.

## Quick Start

```bash
cargo build --bin ag

ag init
ag add "Design storage"
ag add "Implement storage"
ag link <implement-id> --blocks <design-id>
ag ready --json
ag claim-next --actor worker-a --json
```

The command is `ag`; the project and crate are `agent-stride`.

## Durable Runtime Demo

The demo creates a Git-backed plan, starts a run, kills a worker, recovers the
expired lease, evolves the plan through approval, and verifies the final state.

```bash
./scripts/durable-runtime-demo.sh
cargo run --bin ag-eval -- --variant runtime --scenario chain --agents 2 --seed 7
```

For a read-only local execution console:

```bash
./scripts/observability-demo.sh
```

## Checks

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets
cargo bench --bench engine -- --noplot
```

The deterministic evaluator and benchmark report are evidence for local
correctness and behavior, not production-scale performance claims.

## Scope

AgentStride is a single-machine personal project. It does not attempt to be a
Temporal-compatible server or a general Agent platform.

Out of scope:

- distributed scheduling and multi-host failover;
- multi-tenant identity and authorization;
- filesystem, network, or container isolation;
- exactly-once semantics for arbitrary external side effects;
- remote databases and a resident daemon.

## Documentation

- [Architecture](docs/architecture.md)
- [Runtime model](docs/runtime.md)
- [Development and verification](docs/development.md)

## License

MIT
