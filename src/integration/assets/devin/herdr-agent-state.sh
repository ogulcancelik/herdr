#!/bin/sh
# installed by herdr
# managed by herdr; reinstalling or updating the integration overwrites this file.
# add custom hooks beside this file instead of editing it.
# HERDR_INTEGRATION_ID=devin
# HERDR_INTEGRATION_VERSION=1

set -eu

action="${1:-}"
hook_input_file="$(mktemp "${TMPDIR:-/tmp}/herdr-devin-hook.XXXXXX")" || exit 0
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

HERDR_HOOK_INPUT_FILE="$hook_input_file" python3 - <<'PY'
import json
import os
import random
import socket
import subprocess
import time

SOURCE = "herdr:devin"
AGENT = "devin"


def load_hook_input(path: str | None) -> dict:
    if not path:
        return {}
    try:
        with open(path, encoding="utf-8") as handle:
            content = handle.read()
        if not content.strip():
            return {}
        parsed = json.loads(content)
        return parsed if isinstance(parsed, dict) else {}
    except Exception:
        return {}


def load_session_list(project_dir: str | None):
    injected = os.environ.get("HERDR_DEVIN_LIST_JSON")
    if injected is not None:
        try:
            parsed = json.loads(injected)
            return parsed if isinstance(parsed, list) else []
        except Exception:
            return []

    cmd = ["devin", "list", "--format", "json"]
    try:
        result = subprocess.run(
            cmd,
            cwd=project_dir or None,
            capture_output=True,
            text=True,
            timeout=2,
            check=False,
        )
    except Exception:
        return []

    if result.returncode != 0:
        return []

    try:
        parsed = json.loads(result.stdout)
        return parsed if isinstance(parsed, list) else []
    except Exception:
        return []


def normalize_path(path: str | None) -> str | None:
    if not isinstance(path, str) or not path:
        return None
    try:
        return os.path.realpath(path)
    except Exception:
        return path


def hook_session_id(hook_input: dict) -> str | None:
    for key in ("session_id", "sessionId"):
        value = hook_input.get(key)
        if isinstance(value, str) and value:
            return value
    return None


def hook_event_name(hook_input: dict) -> str:
    value = hook_input.get("hook_event_name")
    return value if isinstance(value, str) else ""


def allow_session_list_fallback(hook_input: dict) -> bool:
    event = hook_event_name(hook_input)
    if event == "UserPromptSubmit":
        return False
    if event == "SessionStart" and hook_input.get("source") == "startup":
        return False
    return True


def resolve_session_id(project_dir: str, hook_input: dict) -> str | None:
    direct = hook_session_id(hook_input)
    if direct:
        return direct
    if not allow_session_list_fallback(hook_input):
        return None

    entries = load_session_list(project_dir)
    project_dir = normalize_path(project_dir)
    for entry in entries:
        if not isinstance(entry, dict):
            continue
        session_id = entry.get("id")
        if not isinstance(session_id, str) or not session_id:
            continue
        working_directory = normalize_path(entry.get("working_directory"))
        if working_directory == project_dir:
            return session_id
    return None


pane_id = os.environ.get("HERDR_PANE_ID")
socket_path = os.environ.get("HERDR_SOCKET_PATH")
project_dir = os.environ.get("DEVIN_PROJECT_DIR") or os.getcwd()
hook_input = load_hook_input(os.environ.get("HERDR_HOOK_INPUT_FILE"))

if not pane_id or not socket_path:
    raise SystemExit(0)

request_id = f"{SOURCE}:{int(time.time() * 1000)}:{random.randrange(1_000_000):06d}"
report_seq = time.time_ns()

session_id = resolve_session_id(project_dir, hook_input)
if not session_id:
    raise SystemExit(0)
request = {
    "id": request_id,
    "method": "pane.report_agent_session",
    "params": {
        "pane_id": pane_id,
        "source": SOURCE,
        "agent": AGENT,
        "agent_session_id": session_id,
        "seq": report_seq,
    },
}

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
