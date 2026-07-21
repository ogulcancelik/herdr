#!/bin/sh
# installed by herdr
# managed by herdr; reinstalling or updating the integration overwrites this file.
# add custom hooks beside this file instead of editing it.
# HERDR_INTEGRATION_ID=grok
# HERDR_INTEGRATION_VERSION=1

set -eu

action="${1:-}"
hook_input_file="$(mktemp "${TMPDIR:-/tmp}/herdr-grok-hook.XXXXXX")" || exit 0
trap 'rm -f "$hook_input_file"' EXIT HUP INT TERM
cat >"$hook_input_file" 2>/dev/null || true

case "$action" in
  session) ;;
  *) exit 0 ;;
esac

[ "${HERDR_ENV:-}" = "1" ] || exit 0
[ -n "${HERDR_SOCKET_PATH:-}" ] || exit 0
[ -n "${HERDR_PANE_ID:-}" ] || exit 0
command -v python3 >/dev/null 2>&1 || exit 0

HERDR_ACTION="$action" HERDR_HOOK_INPUT_FILE="$hook_input_file" python3 - <<'PY'
import json
import os
import random
import socket
import time

source = "herdr:grok"
action = os.environ.get("HERDR_ACTION", "")
pane_id = os.environ.get("HERDR_PANE_ID")
socket_path = os.environ.get("HERDR_SOCKET_PATH")
hook_input_file = os.environ.get("HERDR_HOOK_INPUT_FILE")

if not pane_id or not socket_path:
    raise SystemExit(0)

hook_input = {}
if hook_input_file:
    try:
        with open(hook_input_file, encoding="utf-8") as handle:
            content = handle.read()
        if content.strip():
            hook_input = json.loads(content)
    except Exception:
        hook_input = {}

# Grok uses camelCase; accept Claude-style snake_case too.
hook_event_name = str(
    hook_input.get("hookEventName")
    or hook_input.get("hook_event_name")
    or os.environ.get("GROK_HOOK_EVENT")
    or ""
)
# Normalize common aliases to a single form for comparisons.
hook_event_norm = hook_event_name.replace("-", "_").lower()

is_subagent = bool(
    hook_input.get("agent_id")
    or hook_input.get("agentId")
    or hook_input.get("subagent_id")
    or hook_input.get("subagentId")
)
if is_subagent:
    raise SystemExit(0)
if hook_event_norm in ("subagentstop", "subagent_stop", "subagentend", "subagent_end"):
    raise SystemExit(0)

request_id = f"{source}:{int(time.time() * 1000)}:{random.randrange(1_000_000):06d}"
report_seq = time.time_ns()

session_id = (
    hook_input.get("sessionId")
    or hook_input.get("session_id")
    or os.environ.get("GROK_SESSION_ID")
)
agent_session_id = session_id if isinstance(session_id, str) and session_id else None

transcript_path = hook_input.get("transcript_path") or hook_input.get("transcriptPath")
agent_session_path = (
    transcript_path if isinstance(transcript_path, str) and transcript_path else None
)

session_start_source = None
if hook_event_norm in ("sessionstart", "session_start"):
    raw_source = hook_input.get("source")
    if isinstance(raw_source, str) and raw_source:
        session_start_source = raw_source

if agent_session_id:
    params = {
        "pane_id": pane_id,
        "source": source,
        "agent": "grok",
        "seq": report_seq,
        "agent_session_id": agent_session_id,
    }
    if agent_session_path:
        params["agent_session_path"] = agent_session_path
    if session_start_source:
        params["session_start_source"] = session_start_source
    request = {
        "id": request_id,
        "method": "pane.report_agent_session",
        "params": params,
    }
else:
    raise SystemExit(0)

try:
    client = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    client.settimeout(0.5)
    client.connect(socket_path)
    client.sendall((json.dumps(request) + "\n").encode())
    try:
        client.recv(4096)
    except Exception:
        pass
    client.close()
except Exception:
    pass
PY
