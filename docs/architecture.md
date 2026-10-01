# Architecture

AgentStride is a local-first, single-process control plane for long-running
Agent work.

```text
CLI / MCP / Worker / UI
          |
          v
   Application services
          |
    +-----+-------------------+
    |                         |
    v                         v
 Plan layer              Runtime layer
 Git .tasks files        SQLite History + State
 tasks and edges         runs, attempts, leases, timers
    |                         |
    +------------+------------+
                 v
             Reconciler
```

## Boundaries

### Domain and graph

The domain owns issue values, lifecycle transitions, typed edges, revisions,
stable IDs, and error kinds. Scheduling edges are `blocks` and `parent_child`;
`related` is informational. The graph rejects self-edges and cycles at write
time.

### Plan layer

The plan is exported to `.tasks/` as deterministic JSONL and committed to Git.
A Git revision is an immutable `PlanRevision`. Plan proposals use compare-and-
swap against their base revision and require approval before activation.

### Runtime layer

A `Run` executes one selected plan revision. Runtime state is represented by:

- append-only execution events;
- a materialized `MutableState`;
- durable tasks waiting for workers;
- attempts with leases and fencing;
- persistent timers for timeout and retry.

The materialized state is a query optimization. History is authoritative and
can rebuild the state for verification.

### Application and adapters

CLI handlers, MCP tools, the fake worker, the OpenAI provider, and the
read-only UI call the same application and store boundaries. They do not
implement separate scheduling or lifecycle rules.

The runner is a capability gate with a small command policy. It is not a
filesystem or network sandbox.

## Transaction Rules

Every meaningful mutation follows:

```text
validate
  -> decide
  -> append event
  -> apply materialized state
  -> update durable task projections
  -> commit
```

Primary state, event history, and task queue changes are committed in one
SQLite transaction. A failed validation or stale precondition writes nothing.

Worker completion additionally checks:

```text
workspace + run + task + attempt + worker + lease_until
```

This prevents an expired or superseded worker from completing a newer attempt.

## Recovery

Recovery is semantic:

1. load the selected plan revision and runtime snapshot;
2. rebuild or verify state from execution history;
3. reclaim expired leases;
4. fire due timers and schedule retries;
5. resume from the latest checkpoint.

The system does not claim exactly-once execution for arbitrary external
effects. External commands need idempotency keys, durable operation records, or
human confirmation.

## Interfaces

The stable human and automation surface is the `ag` CLI with JSON output.
`ag mcp` exposes a local stdio MCP server. The UI is read-only and exists to
inspect a run, its attempts, recovery events, and plan revision.

No remote service is required.
