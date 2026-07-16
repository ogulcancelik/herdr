"""Hermes plugin installed by Herdr to report agent lifecycle state."""

# HERDR_INTEGRATION_ID=hermes
# HERDR_INTEGRATION_VERSION=4

from __future__ import annotations

import json
import os
import random
import socket
import time

_SOURCE = "herdr:hermes"
_AGENT = "hermes"


def _base_params() -> tuple[str, str] | None:
    if os.environ.get("HERDR_ENV") != "1":
        return None
    pane_id = os.environ.get("HERDR_PANE_ID", "").strip()
    socket_path = os.environ.get("HERDR_SOCKET_PATH", "").strip()
    if not pane_id or not socket_path:
        return None
    return pane_id, socket_path


def _send(method: str, params: dict) -> None:
    base = _base_params()
    if base is None:
        return
    pane_id, socket_path = base
    params = {
        "pane_id": pane_id,
        "source": _SOURCE,
        "agent": _AGENT,
        "seq": time.time_ns(),
        **params,
    }
    request = {
        "id": f"{_SOURCE}:{int(time.time() * 1000)}:{random.randrange(1_000_000):06d}",
        "method": method,
        "params": params,
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


def _session_id(kwargs: dict) -> str | None:
    value = kwargs.get("session_id")
    if isinstance(value, str) and value:
        return value
    return None


def _report(state: str, **kwargs) -> None:
    params = {"state": state}
    session_id = _session_id(kwargs)
    if session_id:
        params["agent_session_id"] = session_id
    _send("pane.report_agent", params)


def _report_session(session_start_source: str, **kwargs) -> None:
    session_id = _session_id(kwargs)
    if not session_id:
        return
    _send(
        "pane.report_agent_session",
        {
            "agent_session_id": session_id,
            "session_start_source": session_start_source,
        },
    )


def _working(**kwargs) -> None:
    _report("working", **kwargs)


def _blocked(**kwargs) -> None:
    _report("blocked", **kwargs)


def _idle(**kwargs) -> None:
    _report("idle", **kwargs)


def _session_reset(**kwargs) -> None:
    _report_session("resume", **kwargs)


def register(ctx):
    ctx.register_hook("on_session_start", _idle)
    ctx.register_hook("on_session_reset", _session_reset)
    ctx.register_hook("pre_llm_call", _working)
    ctx.register_hook("pre_api_request", _working)
    ctx.register_hook("pre_tool_call", _working)
    ctx.register_hook("post_tool_call", _working)
    ctx.register_hook("pre_approval_request", _blocked)
    ctx.register_hook("post_approval_response", _working)
    ctx.register_hook("post_llm_call", _idle)
    ctx.register_hook("on_session_end", _idle)
