#!/usr/bin/env bash
set -euo pipefail

init='{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":1,"clientCapabilities":{},"clientInfo":{"name":"openab-smoke-test","version":"1.0.0"}}}'
response=$(printf '%s\n' "$init" | timeout 20 docker run --rm --network none -i --entrypoint sh "$1" -ec '
  test "$OPENAB_AGENT_COMMAND" = "agy acp"
  command -v agy-acp >/dev/null
  exec agy acp
')

printf '%s\n' "$response" | jq -se 'any(.[]; .id == 1 and .result.agentCapabilities.promptCapabilities.image == true)' >/dev/null
