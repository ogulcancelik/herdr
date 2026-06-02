#!/bin/sh
# installed by herdr
# managed by herdr; reinstalling or updating the integration overwrites this file.
# add custom hooks beside this file instead of editing it.
# HERDR_INTEGRATION_ID=codex
# HERDR_INTEGRATION_VERSION=5

set -eu

action="${1:-}"
hook_input_file="$(mktemp "${TMPDIR:-/tmp}/herdr-codex-hook.XXXXXX")" || exit 0
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

source = "herdr:codex"
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

def send_rpc(request):
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

def resolve_session_title():
    # Priority 1: session_title provided directly in hook input.
    title = hook_input.get("session_title")
    if isinstance(title, str) and title.strip():
        return title.strip()

    # Priority 2: read summary from sessions-index.json in the transcript dir.
    try:
        transcript_path = hook_input.get("transcript_path")
        session_id_val = hook_input.get("session_id")
        if not isinstance(transcript_path, str) or not isinstance(session_id_val, str):
            return None
        import pathlib
        index_path = pathlib.Path(transcript_path).parent / "sessions-index.json"
        with open(index_path, encoding="utf-8") as fh:
            index = json.loads(fh.read())
        sessions = index if isinstance(index, list) else index.get("sessions", [])
        for entry in sessions:
            if entry.get("id") == session_id_val or entry.get("session_id") == session_id_val:
                summary = entry.get("summary") or entry.get("title") or entry.get("name")
                if isinstance(summary, str) and summary.strip():
                    return summary.strip()
    except Exception:
        pass
    return None

request_id = f"{source}:{int(time.time() * 1000)}:{random.randrange(1_000_000):06d}"
report_seq = time.time_ns()
session_id = hook_input.get("session_id")
agent_session_id = session_id if isinstance(session_id, str) and session_id else None
if agent_session_id:
    request = {
        "id": request_id,
        "method": "pane.report_agent_session",
        "params": {
            "pane_id": pane_id,
            "source": source,
            "agent": "codex",
            "seq": report_seq,
            "agent_session_id": agent_session_id,
        },
    }
else:
    raise SystemExit(0)

send_rpc(request)

session_title = resolve_session_title()
if session_title:
    meta_request = {
        "id": f"{source}:meta:{int(time.time() * 1000)}:{random.randrange(1_000_000):06d}",
        "method": "pane.report_metadata",
        "params": {
            "pane_id": pane_id,
            "source": source,
            "agent": "codex",
            "applies_to_source": source,
            "custom_status": session_title,
            "seq": report_seq + 1,
        },
    }
    send_rpc(meta_request)
PY
