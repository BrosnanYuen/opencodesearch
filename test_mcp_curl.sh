#!/usr/bin/env bash
set -euo pipefail

headers_file="$(mktemp)"
init_body_file="$(mktemp)"
trap 'rm -f "$headers_file" "$init_body_file"' EXIT

curl -k -sS -D "$headers_file" https://localhost:9443/ \
  -H 'Content-Type: application/json' \
  -H 'Accept: application/json, text/event-stream' \
  -d '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"curl-test","version":"1.0"}}}' \
  > "$init_body_file"

echo "--- MCP initialize ---"
cat "$init_body_file"

session_id="$(
  awk 'tolower($1) == "mcp-session-id:" {print $2}' "$headers_file" \
    | tr -d '\r' \
    | tail -n 1
)"

if [[ -z "$session_id" ]]; then
  echo "failed to extract mcp-session-id from initialize response headers" >&2
  exit 1
fi

echo
echo "mcp-session-id: $session_id"
echo

curl -k -sS https://localhost:9443/ \
  -H 'Content-Type: application/json' \
  -H 'Accept: application/json, text/event-stream' \
  -H "mcp-session-id: $session_id" \
  -d '{"jsonrpc":"2.0","method":"notifications/initialized"}' \
  > /dev/null

echo

echo "--- MCP tools/call search_code ---"
curl -k -N https://localhost:9443/ \
  -H 'Content-Type: application/json' \
  -H 'Accept: application/json, text/event-stream' \
  -H "mcp-session-id: $session_id" \
  -d '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"search_code","arguments":{"query":"which function handles poweroff","limit":5}}}'
