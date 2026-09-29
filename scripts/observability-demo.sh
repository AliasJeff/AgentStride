#!/usr/bin/env bash
set -euo pipefail

# Run the durable demo and immediately open its read-only observability UI.
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
AG_BIN="${AG_BIN:-}"
PORT="${PORT:-8787}"

if [[ -z "$AG_BIN" ]]; then
    cargo build --quiet --manifest-path "$ROOT/Cargo.toml" --bin ag
    AG_BIN="$ROOT/target/debug/ag"
fi

demo_output="$("$ROOT/scripts/durable-runtime-demo.sh")"
printf '%s\n' "$demo_output"

demo_workspace="$(sed -n 's/^artifacts: //p' <<<"$demo_output")"
if [[ -z "$demo_workspace" || ! -d "$demo_workspace" ]]; then
    echo "durable demo did not leave a usable workspace" >&2
    exit 1
fi

printf 'opening observability UI on http://127.0.0.1:%s\n' "$PORT"
exec "$AG_BIN" \
    --workspace "$demo_workspace" \
    ui \
    --run research-run \
    --port "$PORT"
