#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
VENV="$ROOT/app/backend/.venv"

if [[ ! -x "$VENV/bin/uvicorn" ]]; then
  printf 'Backend dependencies are missing. Run scripts/setup.sh first.\n' >&2
  exit 1
fi

cd "$ROOT/app/backend"
"$VENV/bin/uvicorn" main:app --host 127.0.0.1 --port 8787 &
backend_pid=$!
cd "$ROOT/app/frontend"
npm run dev -- --host 127.0.0.1 &
frontend_pid=$!

cleanup() {
  kill "$backend_pid" "$frontend_pid" 2>/dev/null || true
  wait "$backend_pid" "$frontend_pid" 2>/dev/null || true
}
trap cleanup EXIT INT TERM
wait
