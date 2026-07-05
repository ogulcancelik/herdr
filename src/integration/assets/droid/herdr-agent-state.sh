#!/bin/sh
# installed by herdr
# managed by herdr; reinstalling or updating the integration overwrites this file.
# add custom hooks beside this file instead of editing it.
# HERDR_INTEGRATION_ID=droid
# HERDR_INTEGRATION_VERSION=3

set -eu

action="${1:-}"

case "$action" in
  session) ;;
  *) exit 0 ;;
esac

[ "${HERDR_ENV:-}" = "1" ] || exit 0
[ -n "${HERDR_SOCKET_PATH:-}" ] || exit 0
[ -n "${HERDR_PANE_ID:-}" ] || exit 0
command -v python3 >/dev/null 2>&1 || exit 0

HERDR_ACTION="$action" python3 - 3<&0 <<'PY'
import json
import os
import random
import socket
import time

source = "herdr:droid"
pane_id = os.environ.get("HERDR_PANE_ID")
socket_path = os.environ.get("HERDR_SOCKET_PATH")

if not pane_id or not socket_path:
    raise SystemExit(0)

def read_stdin(timeout=0.2, max_bytes=1024 * 1024):
    deadline = time.monotonic() + timeout
    fd = 3
    chunks = []
    total = 0
    try:
        os.set_blocking(fd, False)
    except Exception:
        return ""
    while total < max_bytes:
        try:
            chunk = os.read(fd, min(65536, max_bytes - total))
        except BlockingIOError:
            if time.monotonic() >= deadline:
                break
            time.sleep(0.01)
            continue
        except Exception:
            break
        if not chunk:
            break
        chunks.append(chunk)
        total += len(chunk)
    return b"".join(chunks).decode("utf-8", errors="replace")

hook_input = {}
try:
    content = read_stdin()
    if content.strip():
        hook_input = json.loads(content)
except Exception:
    hook_input = {}

session_id = hook_input.get("session_id")
if not isinstance(session_id, str) or not session_id:
    raise SystemExit(0)

request_id = f"{source}:{int(time.time() * 1000)}:{random.randrange(1_000_000):06d}"
report_seq = time.time_ns()
request = {
    "id": request_id,
    "method": "pane.report_agent_session",
    "params": {
        "pane_id": pane_id,
        "source": source,
        "agent": "droid",
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
