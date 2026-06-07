#!/usr/bin/env bash
# End-to-end test for Cursor Agent CLI integration (session restore).
set -euo pipefail

HERDR_BIN="${HERDR_BIN:-/tmp/herdr/target/release/herdr}"
SESSION="${HERDR_E2E_SESSION:-cursor-e2e}"
E2E_ROOT="${HERDR_E2E_ROOT:-/tmp/herdr-e2e-cursor}"
CURSOR_DIR="${CURSOR_CONFIG_DIR:-$E2E_ROOT/.cursor}"
LOG="$E2E_ROOT/e2e.log"
SESSION_DIR="${HERDR_CONFIG_DIR:-$HOME/.config/herdr}/sessions/$SESSION"
SESSION_FILE="$SESSION_DIR/session.json"
SERVER_LOG="$SESSION_DIR/herdr-server.log"

mkdir -p "$E2E_ROOT" "$CURSOR_DIR"
exec > >(tee -a "$LOG") 2>&1

echo "=== herdr cursor integration E2E ==="
echo "herdr: $HERDR_BIN ($("$HERDR_BIN" --version 2>/dev/null || echo unknown))"
echo "session: $SESSION"
echo "cursor config: $CURSOR_DIR"

export CURSOR_CONFIG_DIR="$CURSOR_DIR"
export PATH="$HOME/.local/bin:$PATH"
unset HERDR_ENV HERDR_SOCKET_PATH HERDR_PANE_ID || true

command -v cursor-agent >/dev/null || { echo "FAIL: cursor-agent not on PATH"; exit 1; }
command -v python3 >/dev/null || { echo "FAIL: python3 required"; exit 1; }

ensure_server() {
  if "$HERDR_BIN" --session "$SESSION" workspace list >/dev/null 2>&1; then
    return 0
  fi
  echo "--- starting herdr server for session $SESSION ---"
  nohup env CURSOR_CONFIG_DIR="$CURSOR_DIR" PATH="$PATH" \
    "$HERDR_BIN" --session "$SESSION" server \
    >"$E2E_ROOT/server-boot.log" 2>&1 &
  local pid=$!
  for _ in $(seq 1 40); do
    if "$HERDR_BIN" --session "$SESSION" workspace list >/dev/null 2>&1; then
      echo "server ready (pid $pid)"
      return 0
    fi
    sleep 0.25
  done
  echo "FAIL: server did not become ready"
  cat "$E2E_ROOT/server-boot.log" || true
  exit 1
}

stop_server() {
  "$HERDR_BIN" --session "$SESSION" server stop >/dev/null 2>&1 || true
  sleep 1
}

report_session_via_api() {
  local session_id="$1"
  local reported=""
  for n in $(seq 1 30); do
    local pane_id="p_${n}"
    local response
    response=$(python3 - <<PY
import json, socket, os
socket_path = os.path.expanduser("$SESSION_DIR/herdr.sock")
request = {
    "id": "e2e:report",
    "method": "pane.report_agent_session",
    "params": {
        "pane_id": "$pane_id",
        "source": "herdr:cursor",
        "agent": "cursor",
        "seq": 1,
        "agent_session_id": "$session_id",
    },
}
client = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
client.settimeout(1)
client.connect(socket_path)
client.sendall((json.dumps(request) + "\\n").encode())
print(client.recv(4096).decode())
PY
) || continue
    if printf '%s' "$response" | python3 -c "import json,sys; d=json.load(sys.stdin); sys.exit(0 if d.get('result') else 1)" 2>/dev/null; then
      reported="$pane_id"
      break
    fi
  done
  if [[ -z "$reported" ]]; then
    echo "FAIL: could not report agent session to any pane"
    exit 1
  fi
  echo "reported session via API on $reported"
}

echo "--- clean session ---"
stop_server
rm -rf "$SESSION_DIR" "$CURSOR_DIR"
mkdir -p "$CURSOR_DIR"

echo "--- install cursor integration ---"
ensure_server
"$HERDR_BIN" --session "$SESSION" integration install cursor

test -f "$CURSOR_DIR/herdr-agent-state.sh" || { echo "FAIL: hook script missing"; exit 1; }
test -f "$CURSOR_DIR/hooks.json" || { echo "FAIL: hooks.json missing"; exit 1; }
grep -q herdr-agent-state.sh "$CURSOR_DIR/hooks.json" || { echo "FAIL: sessionStart hook missing"; exit 1; }

echo "--- start cursor-agent in herdr pane ---"
START_JSON=$("$HERDR_BIN" --session "$SESSION" agent start cursor-e2e \
  --cwd /tmp/herdr \
  --no-focus \
  -- cursor-agent -p --trust "Reply with exactly the word: pong")

AGENT_NAME=$(printf '%s' "$START_JSON" | python3 -c "import json,sys; d=json.load(sys.stdin); print(d['result']['agent']['name'])")
PANE_ID=$(printf '%s' "$START_JSON" | python3 -c "import json,sys; d=json.load(sys.stdin); print(d['result']['agent']['pane_id'])")
echo "agent=$AGENT_NAME pane=$PANE_ID"

echo "--- report agent session (hook does not fire in cursor-agent -p mode) ---"
SESSION_ID="cursor-e2e-$(date +%s)"
report_session_via_api "$SESSION_ID"
sleep 0.5
PANE_JSON=$("$HERDR_BIN" --session "$SESSION" pane get "$PANE_ID" 2>/dev/null || true)
if [[ -z "$PANE_JSON" ]]; then
  echo "FAIL: pane get returned empty response for $PANE_ID"
  exit 1
fi
printf '%s' "$PANE_JSON" | python3 -c "
import json, sys
pane = json.load(sys.stdin)['result']['pane']
session = pane.get('agent_session')
if not session or session.get('source') != 'herdr:cursor':
    print('FAIL: agent_session not set after report')
    sys.exit(1)
print('agent_session:', json.dumps(session, indent=2))
"

echo "--- wait for cursor-agent to finish ---"
"$HERDR_BIN" --session "$SESSION" agent wait "$AGENT_NAME" --status idle --timeout 120000

echo "session id: $SESSION_ID"

echo "--- verify persisted session.json before restart ---"
for _ in $(seq 1 20); do
  if [[ -f "$SESSION_FILE" ]]; then
    break
  fi
  sleep 0.5
done
python3 - <<PY
import json, sys
from pathlib import Path
p = Path("$SESSION_FILE")
if not p.exists():
    print("FAIL: session.json missing at", p)
    sys.exit(1)
data = json.loads(p.read_text())
found = False
for ws in data.get("workspaces", []):
    for tab in ws.get("tabs", []):
        for pane in (tab.get("panes") or {}).values():
            pas = pane.get("agent_session")
            if pas and pas.get("source") == "herdr:cursor":
                found = True
                print("persisted:", json.dumps(pas, indent=2))
                argv = pane.get("launch_argv") or []
                if argv and argv[0] != "cursor-agent":
                    print("FAIL: launch_argv should start with cursor-agent, got", argv)
                    sys.exit(1)
if not found:
    print("FAIL: no herdr:cursor agent_session in session.json")
    sys.exit(1)
PY

echo "--- verify resume plan (unit test) ---"
HERDR_REPO="${HERDR_REPO:-/tmp/herdr}"
if [[ -f "$HERDR_REPO/Cargo.toml" ]]; then
  (
    cd "$HERDR_REPO"
    source "$HOME/.cargo/env" 2>/dev/null || true
    cargo test planner_builds_resume_argv_for_official_agents --locked -- --exact >/dev/null
  )
  echo "resume argv unit test passed (cursor-agent --resume <id>)"
else
  echo "note: skipped cargo test; expected argv: cursor-agent --resume $SESSION_ID"
fi

echo "--- stop and restart session ---"
stop_server
ensure_server

echo "--- verify restored agent metadata ---"
AGENT_LIST=$("$HERDR_BIN" --session "$SESSION" agent list)
printf '%s' "$AGENT_LIST" | python3 -c "
import json, sys
d = json.load(sys.stdin)
agents = d['result']['agents']
cursor_agents = [a for a in agents if a.get('agent') == 'cursor' or a.get('name') == 'cursor-e2e']
if not cursor_agents:
    print('FAIL: no restored cursor agent')
    sys.exit(1)
session = cursor_agents[0].get('agent_session')
if not session or session.get('source') != 'herdr:cursor':
    print('FAIL: restored agent missing herdr:cursor session')
    sys.exit(1)
print('restored agent_session:', json.dumps(session, indent=2))
"

echo "--- note on live resume spawn ---"
echo "cursor-agent --resume runs when a Herdr TUI client attaches (headless server alone defers pending resume)."
echo "Manual check: herdr --session $SESSION  # then confirm pane runs cursor-agent --resume $SESSION_ID"

echo "=== E2E PASSED ==="
