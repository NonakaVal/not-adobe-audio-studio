#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN_DIR="${BIN_DIR:-$HOME/.local/bin}"
COMMAND_PATH="$BIN_DIR/podcast-studio"
WRAPPER="$(printf '#!/usr/bin/env bash\nexec %q "$@"\n' "$ROOT/scripts/dev.sh")"

mkdir -p "$BIN_DIR"

if [[ -e "$COMMAND_PATH" || -L "$COMMAND_PATH" ]]; then
  if [[ ! -f "$COMMAND_PATH" || "$(<"$COMMAND_PATH")" != "$WRAPPER" ]]; then
    printf 'Cannot install podcast-studio: %s already exists and is not managed by this project.\n' "$COMMAND_PATH" >&2
    exit 1
  fi
else
  printf '%s' "$WRAPPER" > "$COMMAND_PATH"
fi

chmod +x "$COMMAND_PATH"
printf 'Installed podcast-studio at %s\n' "$COMMAND_PATH"

case ":${PATH}:" in
  *":$BIN_DIR:"*) ;;
  *) printf 'Add %s to your PATH to run podcast-studio from any directory.\n' "$BIN_DIR" ;;
esac
