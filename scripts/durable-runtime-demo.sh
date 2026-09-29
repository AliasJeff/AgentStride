#!/usr/bin/env bash
set -euo pipefail

# Reproducible durable runtime demo. Requires git, jq, and a built ag binary.
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
AG_BIN="${AG_BIN:-$ROOT/target/debug/ag}"
# Keep the generated workspace by default so the following UI command remains
# usable after the demo exits. Set KEEP_DEMO=0 for ephemeral CI-style runs.
KEEP_DEMO="${KEEP_DEMO:-1}"

command -v git >/dev/null || {
    echo "git is required" >&2
    exit 2
}
command -v jq >/dev/null || {
    echo "jq is required" >&2
    exit 2
}

if [[ ! -x "$AG_BIN" ]]; then
    cargo build --quiet --manifest-path "$ROOT/Cargo.toml" --bin ag
fi

DEMO_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/agent-stride-runtime.XXXXXX")"
cleanup() {
    if [[ "$KEEP_DEMO" != "1" ]]; then
        rm -rf "$DEMO_ROOT"
    fi
}
trap cleanup EXIT

cd "$DEMO_ROOT"
run_ag() {
    "$AG_BIN" --workspace "$DEMO_ROOT" "$@"
}

future_timestamp() {
    if date -u -v+5S +%Y-%m-%dT%H:%M:%SZ >/dev/null 2>&1; then
        date -u -v+5S +%Y-%m-%dT%H:%M:%SZ
    else
        date -u -d "+5 seconds" +%Y-%m-%dT%H:%M:%SZ
    fi
}

run_ag init --json >/dev/null
git init -q
git config user.email "agent-stride-demo@example.com"
git config user.name "AgentStride Demo"

source_a="$(run_ag add "collect source A" --description "Collect and normalize source A" --json)"
source_a_id="$(jq -r '.id' <<<"$source_a")"

run_ag plan export --json >/dev/null
git add .tasks
git commit -qm "demo: initial research plan"
base_revision="$(git rev-parse HEAD)"

run_ag runtime start research-run --plan "$base_revision" --json >/dev/null
run_ag reconcile research-run "$base_revision" --json >/dev/null

poll_json="$(run_ag worker poll \
    --worker crashed-worker \
    --lease-until "$(future_timestamp)" \
    --json)"
stale_token="$(jq -r '.token' <<<"$poll_json")"
crashed_durable_task="$(jq -r '.lease.durable_task_id' <<<"$poll_json")"

# The child has acquired a real durable lease, then is terminated before it can
# acknowledge completion. Recovery must make the attempt runnable again.
(
    run_ag worker start "$stale_token" --json >/dev/null
    sleep 60
) &
worker_pid=$!
sleep 6
kill -9 "$worker_pid" 2>/dev/null || true
wait "$worker_pid" 2>/dev/null || true

recovery_json="$(run_ag runtime recover research-run --retry-delay 0 --json)"
recovered_durable_task="$(jq -r '.recovered[0]' <<<"$recovery_json")"
run_ag runtime fire "timer-${recovered_durable_task}" --json >/dev/null

set +e
run_ag worker complete "$stale_token" --result '{"stale":true}' --json >/dev/null 2>&1
stale_completion_status=$?
set -e

# Evolve the plan after the crash and require approval before activation.
source_b="$(run_ag add "collect source B" --description "Collect and normalize source B" --json)"
source_b_id="$(jq -r '.id' <<<"$source_b")"
run_ag plan export --json >/dev/null
git add .tasks
git commit -qm "demo: evolve research plan"
head_revision="$(git rev-parse HEAD)"

run_ag plan propose research-run \
    --base "$base_revision" \
    --head "$head_revision" \
    --author planner \
    --reason "add a second independent source after restart" \
    --id research-proposal \
    --json >/dev/null
run_ag plan approve research-proposal --actor reviewer --json >/dev/null
run_ag plan activate research-run "$head_revision" --json >/dev/null

# Drain the durable queue with the deterministic worker.
for _ in $(seq 1 10); do
    worker_result="$(run_ag worker run --worker replacement-worker --provider fake --seed 7 --json)"
    [[ "$(jq -r '.type' <<<"$worker_result")" == "idle" ]] && break
done

events_json="$(run_ag runtime events research-run --since 0 --limit 10000 --json)"
state_json="$(run_ag runtime show research-run --json)"
git_log="$(git log --oneline --all -- .tasks)"

metrics_json="$(
    jq -n \
        --argjson events "$(jq '.events' <<<"$events_json")" \
        --argjson state "$(jq '.' <<<"$state_json")" \
        --argjson stale_status "$stale_completion_status" \
        --arg crashed_durable_task "$crashed_durable_task" \
        --arg recovered_durable_task "$recovered_durable_task" \
        --arg base_revision "$base_revision" \
        --arg head_revision "$head_revision" \
        --arg source_a_id "$source_a_id" \
        --arg source_b_id "$source_b_id" \
        '{
            schema_version: 1,
            run_id: $state.run_id,
            base_revision: $base_revision,
            head_revision: $head_revision,
            plan_revision: $state.plan_revision,
            history_version: $state.history_version,
            task_count: ($state.tasks | length),
            succeeded_tasks: ([$state.tasks[] | select(.status == "succeeded")] | length),
            claim_events: ([$events[] | select(.kind.type == "task_claimed")] | length),
            successful_events: ([$events[] | select(.kind.type == "task_succeeded")] | length),
            retry_events: ([$events[] | select(.kind.type == "task_retryable_failed")] | length),
            timeout_events: ([$events[] | select(.kind.type == "task_timed_out")] | length),
            checkpoint_events: ([$events[] | select(.kind.type == "task_checkpointed")] | length),
            plan_revision_events: ([$events[] | select(.kind.type == "plan_revision_activated")] | length),
            duplicate_claim_events: (
                [$events | map(select(.kind.type == "task_claimed"))
                    | group_by(.kind.attempt_id)
                    | map(select(length > 1))
                    | length][0]
            ),
            lost_events_observed: (
                if ($state.history_version == ($events | length)) then 0 else 1 end
            ),
            stale_worker_successes: (if $stale_status == 0 then 1 else 0 end),
            crashed_durable_task: $crashed_durable_task,
            recovered_durable_task: $recovered_durable_task,
            task_ids: [$source_a_id, $source_b_id],
            verification: {
                stale_worker_rejected: ($stale_status != 0),
                plan_evolved: ($base_revision != $head_revision
                    and $state.plan_revision == $head_revision),
                all_tasks_succeeded: ([ $state.tasks[] | .status ] | all(. == "succeeded"))
            }
        }'
)"

printf '%s\n' "$events_json" > runtime-events.json
printf '%s\n' "$state_json" > runtime-state.json
printf '%s\n' "$metrics_json" > metrics.json
printf '%s\n' "$git_log" > plan-git-log.txt

if [[ "$stale_completion_status" -eq 0 ]]; then
    echo "stale worker unexpectedly completed a task" >&2
    exit 1
fi
if [[ "$(jq -r '.verification.all_tasks_succeeded' metrics.json)" != "true" ]]; then
    echo "demo did not complete every task" >&2
    exit 1
fi

printf 'durable runtime demo passed\n'
printf 'artifacts: %s\n' "$DEMO_ROOT"
printf 'ui command: cargo run --bin ag -- --workspace %q ui --run research-run --port 8787\n' \
    "$DEMO_ROOT"
cat metrics.json
