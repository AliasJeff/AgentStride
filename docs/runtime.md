# Runtime Model

AgentStride is built around one distinction:

```text
Plan says what should happen.
History says what did happen.
```

An LLM plan is mutable and non-deterministic. It is therefore stored as data,
not treated as replayable workflow code.

## Objects

| Object | Meaning |
| --- | --- |
| `PlanRevision` | Immutable Git revision containing tasks and edges |
| `Run` | One execution of a selected plan revision |
| `RunTask` | Materialized task state inside a run |
| `Attempt` | One concrete worker execution |
| `Event` | Durable fact about a committed runtime transition |
| `DurableTask` | Persistent unit available for worker polling |
| `Checkpoint` | Resumable progress for the active attempt |
| `Timer` | Persistent timeout or retry trigger |
| `Proposal` | Requested plan change tied to a base revision |

## State Machine

The runtime follows:

```text
Command -> validate -> decide -> Event -> apply
```

Typical task flow:

```text
pending
  -> ready
  -> scheduled
  -> claimed
  -> running
  -> checkpointed
  -> succeeded
```

Failure paths are explicit:

```text
running -> waiting
running -> retry timer -> ready
running -> timed out -> retry timer
running -> failed
running -> handed off -> new attempt
```

Completed tasks are not silently rerun when a plan changes. A removed or
replaced task becomes an explicit invalidation or supersession event.

## Worker Safety

Workers poll durable tasks and receive a signed capability token. The token is
bound to the workspace, run, durable task, task, attempt, worker, and lease.

Every heartbeat, checkpoint, handoff, retry, and completion validates the
current lease and attempt. A stale token fails instead of changing state.

## Plan Evolution

```text
current revision
      |
      v
proposal(base revision, candidate revision)
      |
      v
approval
      |
      v
reconcile selected Run
```

Reconciliation computes newly discovered work, ready work, invalidations, and
explicit task supersessions. It preserves execution history and never treats a
new plan as permission to rewrite old outcomes.

## Agent Context

The worker context is a bounded projection of durable state:

- current plan revision;
- task identity and attempt number;
- latest checkpoint;
- recent execution events;
- relevant project memory and task context.

`prime`, hooks, checkpoints, memory, messages, and MCP are adapters around the
same source of truth. They do not replace runtime history.

## Evaluation

The built-in fake worker makes control-plane behavior deterministic. It can
exercise chains, diamonds, retries, timeouts, handoffs, recovery, and plan
evolution without a model API key.

```bash
cargo run --bin ag-eval -- --scenario chain --agents 2 --seed 7
cargo run --bin ag-eval -- --variant runtime --scenario chain --agents 2 --seed 7
```

The important measures are dependency violations, duplicate claims, successful
recovery, and history verification.
