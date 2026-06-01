#!/bin/sh
# installed by herdr
# managed by herdr; reinstalling or updating the integration overwrites this file.
# add custom hooks beside this file instead of editing it.
# High-level state machine:
# - SessionStart reports working when Copilot includes an initial prompt, else idle.
# - UserPromptSubmit reports working for the active turn.
# - PreToolUse reports blocked for ask_user and exit_plan_mode, else working.
# - notification permission_prompt / elicitation_dialog reports blocked.
# - PostToolUse / PostToolUseFailure clears handled blockers back to working.
# - agentStop / Stop with end_turn reports idle.
# - SessionEnd only releases ownership for real exits such as user_exit / abort.
# Official Copilot hooks reference:
# https://docs.github.com/en/copilot/reference/hooks-reference
# HERDR_INTEGRATION_ID=copilot
# HERDR_INTEGRATION_VERSION=1

set -eu

hook_input_file="$(mktemp "${TMPDIR:-/tmp}/herdr-copilot-hook.XXXXXX")" || exit 0
trap 'rm -f "$hook_input_file"' EXIT HUP INT TERM
cat >"$hook_input_file" 2>/dev/null || true

[ "${HERDR_ENV:-}" = "1" ] || exit 0
[ -n "${HERDR_SOCKET_PATH:-}" ] || exit 0
[ -n "${HERDR_PANE_ID:-}" ] || exit 0
command -v python3 >/dev/null 2>&1 || exit 0

HERDR_HOOK_INPUT_FILE="$hook_input_file" python3 - <<'PY'
import json
import os
import random
import socket
import time

source = "herdr:copilot"
agent = "copilot"
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
            parsed = json.loads(content)
            if isinstance(parsed, dict):
                hook_input = parsed
    except Exception:
        hook_input = {}

def first_text(*keys):
    for key in keys:
        value = hook_input.get(key)
        if isinstance(value, str) and value:
            return value
    return None

def normalize_event(event):
    return event.replace("_", "").replace("-", "").lower()

def infer_event():
    explicit = first_text("hook_event_name", "hookEventName")
    if explicit:
        return explicit
    if first_text("notification_type", "notificationType"):
        return "notification"
    if "toolResult" in hook_input or "tool_result" in hook_input:
        return "postToolUse"
    if "error" in hook_input and first_text("tool_name", "toolName"):
        return "postToolUseFailure"
    if first_text("tool_name", "toolName"):
        return "preToolUse"
    if first_text("stop_reason", "stopReason"):
        return "agentStop"
    if first_text("reason"):
        return "sessionEnd"
    if "prompt" in hook_input:
        return "userPromptSubmitted"
    if (
        "initial_prompt" in hook_input
        or "initialPrompt" in hook_input
        or "source" in hook_input
        or first_text("session_id", "sessionId")
    ):
        return "sessionStart"
    return ""

def has_initial_prompt():
    value = hook_input.get("initial_prompt", hook_input.get("initialPrompt"))
    return isinstance(value, str) and bool(value.strip())

def is_user_blocking_tool(name):
    return name in ("ask_user", "exit_plan_mode")

def is_ignored_post_tool(name):
    # report_intent can arrive after ask_user has already blocked the turn but
    # before the user answers. Suppress it so it cannot clear that blocker.
    return name in ("report_intent",)

def send(method, params):
    request = {
        "id": f"{source}:{int(time.time() * 1000)}:{random.randrange(1_000_000):06d}",
        "method": method,
        "params": {
            "pane_id": pane_id,
            "source": source,
            "agent": agent,
            "seq": time.time_ns(),
            **params,
        },
    }
    try:
        client = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        client.settimeout(0.5)
        client.connect(socket_path)
        client.sendall((json.dumps(request) + "\n").encode("utf-8"))
        try:
            client.recv(4096)
        except Exception:
            pass
        client.close()
    except Exception:
        pass

def report(state, session_id):
    params = {"state": state}
    if session_id:
        params["agent_session_id"] = session_id
    send("pane.report_agent", params)

def release():
    send("pane.release_agent", {})

try:
    event = infer_event()
    event_key = normalize_event(event)
    session_id = first_text("session_id", "sessionId")
    tool_name = first_text("tool_name", "toolName")
    notification_type = first_text("notification_type", "notificationType")
    stop_reason = first_text("stop_reason", "stopReason")
    reason = first_text("reason")

    if event_key == "sessionstart":
        report("working" if has_initial_prompt() else "idle", session_id)
    elif event_key in ("userpromptsubmit", "userpromptsubmitted"):
        report("working", session_id)
    elif event_key == "pretooluse":
        report("blocked" if is_user_blocking_tool(tool_name) else "working", session_id)
    elif event_key in ("posttooluse", "posttoolusefailure"):
        if is_ignored_post_tool(tool_name):
            pass
        else:
            report("working", session_id)
    elif event_key == "notification":
        if notification_type in ("permission_prompt", "elicitation_dialog"):
            report("blocked", session_id)
        elif notification_type == "agent_idle":
            report("idle", session_id)
        else:
            pass
    elif event_key in ("stop", "agentstop", "sessionstop"):
        if not stop_reason or stop_reason == "end_turn":
            report("idle", session_id)
        else:
            pass
    elif event_key == "sessionend":
        # Copilot can emit reason=complete at normal turn boundaries. Keep
        # ownership until a real session exit so Herdr can still track later turns.
        if reason in ("user_exit", "abort"):
            release()
        else:
            pass
    else:
        pass
except Exception:
    pass
PY
