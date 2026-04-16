#!/bin/sh
set -eu

GEMINI_DIR="${HOME:-/home/node}/.gemini"
PROJECTS_JSON="${GEMINI_DIR}/projects.json"

# Gemini CLI expects a registry-shaped JSON object at startup.
# Keep existing files when they already advertise the registry shape.
mkdir -p "$GEMINI_DIR"
if [ ! -f "$PROJECTS_JSON" ] || ! grep -q '"projects"[[:space:]]*:' "$PROJECTS_JSON"; then
  printf '{"projects":{}}\n' > "$PROJECTS_JSON"
fi

exec "$@"
