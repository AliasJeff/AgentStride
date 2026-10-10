#!/usr/bin/env bash
set -euo pipefail

# Run the durable demo and immediately open its read-only observability UI.
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
AS_BIN="${AS_BIN:-}"
PORT="${PORT:-8787}"

if [[ -z "$AS_BIN" ]]; then
    if [[ -x "$ROOT/target/debug/as" ]]; then
        AS_BIN="$ROOT/target/debug/as"
    elif [[ -x "$ROOT/as" ]]; then
        AS_BIN="$ROOT/as"
    fi
fi

if [[ -z "$AS_BIN" || ! -x "$AS_BIN" ]]; then
    cargo build --quiet --manifest-path "$ROOT/Cargo.toml" --bin as
    AS_BIN="$ROOT/target/debug/as"
fi

demo_output="$("$ROOT/scripts/durable-runtime-demo.sh")"
printf '%s\n' "$demo_output"

demo_workspace="$(sed -n 's/^artifacts: //p' <<<"$demo_output")"
if [[ -z "$demo_workspace" || ! -d "$demo_workspace" ]]; then
    echo "durable demo did not leave a usable workspace" >&2
    exit 1
fi

printf 'opening observability UI on http://127.0.0.1:%s\n' "$PORT"
exec "$AS_BIN" \
    --workspace "$demo_workspace" \
    ui \
    --run research-run \
    --port "$PORT"
