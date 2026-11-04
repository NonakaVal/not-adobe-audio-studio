#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PYTHON="${PYTHON:-python3}"

"$PYTHON" -m venv "$ROOT/app/backend/.venv"
"$ROOT/app/backend/.venv/bin/python" -m pip install --upgrade pip
"$ROOT/app/backend/.venv/bin/python" -m pip install -e "$ROOT/app/backend[test,transcription]"
npm --prefix "$ROOT/app/frontend" install
"$ROOT/scripts/install-global.sh"

printf '\nSetup complete. Start the app with: podcast-studio\n'
printf 'For local transcription, use Python 3.12 and install app/backend[transcription].\n'
