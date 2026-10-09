#!/usr/bin/env bash
set -euo pipefail

# Reproducible parallel-branches demo. Requires git, jq, and a built as binary.
#
# The plan is a release DAG with real branching:
#
#                draft release plan              (wave 1)
#               /       |         \
#      build backend  build frontend  write docs (wave 2, 3-way parallel)
#               \       /
#             run integration tests             (wave 3, diamond join)
#                    \        |
#                     cut release <-----------  (wave 4, fan-in join)
#
# Three workers claim the three wave-2 branches concurrently. One branch fails
# transiently, parks behind a durable retry timer, and recovers without losing
# the other two branches. Reconciles prove downstream work stays gated until
# every predecessor has succeeded.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
AS_BIN="${AS_BIN:-$ROOT/target/debug/as}"
# Keep the generated workspace by default so the UI command stays usable.
# Set KEEP_DEMO=0 for ephemeral CI-style runs.
KEEP_DEMO="${KEEP_DEMO:-1}"

command -v git >/dev/null || {
    echo "git is required" >&2
    exit 2
}
command -v jq >/dev/null || {
    echo "jq is required" >&2
    exit 2
}

if [[ ! -x "$AS_BIN" ]]; then
    cargo build --quiet --manifest-path "$ROOT/Cargo.toml" --bin as
fi

DEMO_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/agent-stride-branches.XXXXXX")"
WORKERS_DIR="$(mktemp -d "${TMPDIR:-/tmp}/agent-stride-branch-workers.XXXXXX")"
cleanup() {
    if [[ "$KEEP_DEMO" != "1" ]]; then
        rm -rf "$DEMO_ROOT" "$WORKERS_DIR"
    fi
}
trap cleanup EXIT

cd "$DEMO_ROOT"
run_as() {
    "$AS_BIN" --workspace "$DEMO_ROOT" "$@"
}

RUN_ID=release-run

scheduled_from_reconcile() {
    jq -c '[.events[] | select(.kind.type == "task_scheduled") | .kind.task_id] | sort' <<<"$1"
}

assert_eq() {
    local actual="$1" expected="$2" message="$3"
    if [[ "$actual" != "$expected" ]]; then
        echo "assertion failed: $message" >&2
        echo "  expected: $expected" >&2
        echo "  actual:   $actual" >&2
        exit 1
    fi
}

run_as init --json >/dev/null
git init -q
git config user.email "agent-stride-demo@example.com"
git config user.name "AgentStride Demo"

root_id="$(jq -r '.id' <<<"$(run_as add "draft release plan" --description "Decide scope and acceptance for the release" --json)")"
backend_id="$(jq -r '.id' <<<"$(run_as add "build backend" --description "Implement the backend branch" --json)")"
frontend_id="$(jq -r '.id' <<<"$(run_as add "build frontend" --description "Implement the frontend branch" --json)")"
docs_id="$(jq -r '.id' <<<"$(run_as add "write docs" --description "Document the release in parallel" --json)")"
tests_id="$(jq -r '.id' <<<"$(run_as add "run integration tests" --description "Diamond join: backend AND frontend" --json)")"
release_id="$(jq -r '.id' <<<"$(run_as add "cut release" --description "Fan-in join: tests AND docs" --json)")"

run_as link "$backend_id" --blocks "$root_id" --json >/dev/null
run_as link "$frontend_id" --blocks "$root_id" --json >/dev/null
run_as link "$docs_id" --blocks "$root_id" --json >/dev/null
run_as link "$tests_id" --blocks "$backend_id" --json >/dev/null
run_as link "$tests_id" --blocks "$frontend_id" --json >/dev/null
run_as link "$release_id" --blocks "$tests_id" --json >/dev/null
run_as link "$release_id" --blocks "$docs_id" --json >/dev/null

run_as plan export --json >/dev/null
git add .tasks
git commit -qm "demo: branching release plan"
base_revision="$(git rev-parse HEAD)"

# --- Wave 1: only the root is executable ------------------------------------
run_as runtime start "$RUN_ID" --plan "$base_revision" --json >/dev/null
wave1="$(run_as reconcile "$RUN_ID" "$base_revision" --json)"
wave1_scheduled="$(scheduled_from_reconcile "$wave1")"
assert_eq "$wave1_scheduled" "[\"$root_id\"]" "wave 1 schedules only the root"

run_as worker run --worker plan-worker --provider fake --seed 7 --json >/dev/null

# --- Wave 2: three branches become ready together ----------------------------
wave2="$(run_as reconcile "$RUN_ID" "$base_revision" --json)"
wave2_scheduled="$(scheduled_from_reconcile "$wave2")"
assert_eq "$wave2_scheduled" \
    "$(jq -cn --arg b "$backend_id" --arg d "$docs_id" --arg f "$frontend_id" '[$b, $d, $f] | sort')" \
    "wave 2 schedules exactly the three branches"

# Three workers race for the wave simultaneously; the first worker fails its
# attempt on purpose, so whichever branch it grabbed parks behind a durable
# retry timer while the other two branches run to completion in parallel.
(
    run_as worker run --worker branch-worker-a --provider fake --seed 7 \
        --failures-before-success 1 --json >"$WORKERS_DIR/a.json" 2>&1
) &
(
    run_as worker run --worker branch-worker-b --provider fake --seed 7 --json \
        >"$WORKERS_DIR/b.json" 2>&1
) &
(
    run_as worker run --worker branch-worker-c --provider fake --seed 7 --json \
        >"$WORKERS_DIR/c.json" 2>&1
) &
wait

claimed="$(cat "$WORKERS_DIR/a.json" "$WORKERS_DIR/b.json" "$WORKERS_DIR/c.json" |
    jq -cs '[.[].task_id] | sort')"
assert_eq "$claimed" "$wave2_scheduled" \
    "three concurrent workers claimed the three distinct branches"

failed_type="$(jq -r '.type' <"$WORKERS_DIR/a.json")"
assert_eq "$failed_type" "retry_scheduled" \
    "one branch failed transiently and scheduled a durable retry"
retry_task_id="$(jq -r '.task_id' <"$WORKERS_DIR/a.json")"
retry_at="$(jq -r '.retry_at' <"$WORKERS_DIR/a.json")"

# --- Dependency gate: nothing downstream unlocks while the branch waits ------
gate="$(run_as reconcile "$RUN_ID" "$base_revision" --json)"
gate_scheduled="$(scheduled_from_reconcile "$gate")"
assert_eq "$gate_scheduled" "[]" \
    "downstream work stays gated while one branch sits behind a retry timer"

# --- Fire the durable retry timer and finish the parked branch ---------------
timer_id="$(run_as runtime show "$RUN_ID" --json | jq -r '.timers | keys | .[0]')"
retry_epoch="$(jq -rn --arg t "$retry_at" \
    '($t | sub("\\.[0-9]+"; "") | strptime("%Y-%m-%dT%H:%M:%SZ") | mktime)')"
now_epoch="$(date +%s)"
if ((retry_epoch > now_epoch)); then
    sleep $((retry_epoch - now_epoch))
fi
run_as runtime fire "$timer_id" --json >/dev/null
retry_result="$(run_as worker run --worker branch-worker-a --provider fake --seed 7 --json)"
assert_eq "$(jq -r '.type' <<<"$retry_result")" "completed" \
    "the parked branch completed on its retry attempt"
assert_eq "$(jq -r '.task_id' <<<"$retry_result")" "$retry_task_id" \
    "the retry resumed the exact parked branch"

# --- Wave 3: the diamond join unlocks after both build branches succeed ------
wave3="$(run_as reconcile "$RUN_ID" "$base_revision" --json)"
wave3_scheduled="$(scheduled_from_reconcile "$wave3")"
assert_eq "$wave3_scheduled" "[\"$tests_id\"]" \
    "integration tests unlock only after backend AND frontend succeeded"
run_as worker run --worker tests-worker --provider fake --seed 7 --json >/dev/null

# --- Wave 4: the fan-in join -------------------------------------------------
wave4="$(run_as reconcile "$RUN_ID" "$base_revision" --json)"
wave4_scheduled="$(scheduled_from_reconcile "$wave4")"
assert_eq "$wave4_scheduled" "[\"$release_id\"]" \
    "the release unlocks only after tests AND docs succeeded"
run_as worker run --worker release-worker --provider fake --seed 7 --json >/dev/null

events_json="$(run_as runtime events "$RUN_ID" --since 0 --limit 10000 --json)"
state_json="$(run_as runtime show "$RUN_ID" --json)"

metrics_json="$(
    jq -n \
        --argjson state "$(jq '.' <<<"$state_json")" \
        --argjson events "$(jq '.events' <<<"$events_json")" \
        --argjson waves "[$wave1_scheduled,$wave2_scheduled,$gate_scheduled,$wave3_scheduled,$wave4_scheduled]" \
        --arg backend_id "$backend_id" \
        --arg frontend_id "$frontend_id" \
        --arg tests_id "$tests_id" \
        --arg release_id "$release_id" \
        --arg base_revision "$base_revision" \
        '{
            schema_version: 1,
            run_id: $state.run_id,
            base_revision: $base_revision,
            plan_revision: $state.plan_revision,
            history_version: $state.history_version,
            task_count: ($state.tasks | length),
            succeeded_tasks: ([$state.tasks[] | select(.status == "succeeded")] | length),
            waves: $waves,
            max_wave_parallelism: ($waves | map(length) | max),
            distinct_workers: ([$events[] | select(.kind.type == "task_claimed") | .kind.worker] | unique),
            claims: ([$events[] | select(.kind.type == "task_claimed")] | length),
            retries: ([$events[] | select(.kind.type == "task_retryable_failed")] | length),
            timers_created: ([$events[] | select(.kind.type == "timer_created")] | length),
            timers_fired: ([$events[] | select(.kind.type == "timer_fired")] | length),
            lost_events_observed: (
                if ($state.history_version == ($events | length)) then 0 else 1 end
            ),
            verification: {
                all_tasks_succeeded: ([$state.tasks[] | .status] | all(. == "succeeded")),
                branches_ran_concurrently: ($waves[1] | length == 3),
                gated_wave_between_retries: ($waves[2] | length == 0),
                diamond_join_gated: (
                    ([$events[] | select(.kind.type == "task_scheduled" and .kind.task_id == $tests_id)][0].seq) >
                    ([$events[] | select(.kind.type == "task_succeeded" and .kind.task_id == $backend_id)][0].seq) and
                    ([$events[] | select(.kind.type == "task_scheduled" and .kind.task_id == $tests_id)][0].seq) >
                    ([$events[] | select(.kind.type == "task_succeeded" and .kind.task_id == $frontend_id)][0].seq)
                ),
                fan_in_join_gated: (
                    ([$events[] | select(.kind.type == "task_scheduled" and .kind.task_id == $release_id)][0].seq) >
                    ([$events[] | select(.kind.type == "task_succeeded" and .kind.task_id == $tests_id)][0].seq)
                ),
                retried_branch_recovered: (
                    [$state.tasks[] | select(.attempt >= 2 and .status == "succeeded")] | length == 1
                )
            }
        }'
)"

printf '%s\n' "$events_json" > runtime-events.json
printf '%s\n' "$state_json" > runtime-state.json
printf '%s\n' "$metrics_json" > metrics.json

if [[ "$(jq -r '.verification.all_tasks_succeeded' metrics.json)" != "true" ]]; then
    echo "demo did not complete every task" >&2
    exit 1
fi
if [[ "$(jq -r '.lost_events_observed' metrics.json)" != "0" ]]; then
    echo "history version diverged from the durable event count" >&2
    exit 1
fi
for gate_name in branches_ran_concurrently gated_wave_between_retries diamond_join_gated fan_in_join_gated retried_branch_recovered; do
    if [[ "$(jq -r ".verification.$gate_name" metrics.json)" != "true" ]]; then
        echo "verification failed: $gate_name" >&2
        exit 1
    fi
done

printf 'parallel branches demo passed\n'
printf 'artifacts: %s\n' "$DEMO_ROOT"
printf 'ui command: cargo run --bin as -- --workspace %q ui --run %q --port 8787\n' \
    "$DEMO_ROOT" "$RUN_ID"
cat metrics.json
