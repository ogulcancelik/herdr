#!/bin/sh
# installed by herdr
# HERDR_INTEGRATION_ID=qodercli
# HERDR_INTEGRATION_VERSION=1
#
# This hook reports qodercli agent state changes to herdr.
# It is registered as a Command hook in ~/.qoder/settings.json
# and invoked by qodercli's hook system on lifecycle events.
#
# Hook events mapped to herdr states:
#   SessionStart   -> working
#   PreToolUse     -> working (tool about to execute)
#   PostToolUse    -> idle (tool completed, back to waiting)
#   SessionEnd     -> idle
#   Stop           -> idle

set -eu

# Only run inside herdr-managed panes
[ "${HERDR_ENV:-}" = "1" ] || exit 0
[ -n "${HERDR_SOCKET_PATH:-}" ] || exit 0
[ -n "${HERDR_PANE_ID:-}" ] || exit 0

# The hook event name is passed via QODER_HOOK_EVENT env var by qodercli
hook_event="${QODER_HOOK_EVENT:-}"

case "$hook_event" in
  SessionStart|PreToolUse)
    state="working"
    ;;
  PostToolUse|SessionEnd|Stop)
    state="idle"
    ;;
  *)
    # Unknown event, don't report
    exit 0
    ;;
esac

pane_id="$HERDR_PANE_ID"
socket_path="$HERDR_SOCKET_PATH"

# Generate a simple request ID
request_id="herdr_qodercli_$$_$(date +%s)"

# Build JSON request
json=$(cat <<EOF
{"id":"${request_id}","method":"pane.report_agent","params":{"pane_id":"${pane_id}","source":"herdr:qodercli","agent":"qodercli","state":"${state}"}}
EOF
)

# Send via Unix socket using available tools
if command -v python3 >/dev/null 2>&1; then
  python3 -c "
import socket, sys
s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
try:
    s.connect('${socket_path}')
    s.sendall(b'${json}\n')
    s.close()
except Exception:
    pass
" 2>/dev/null || true
elif command -v socat >/dev/null 2>&1; then
  printf '%s\n' "$json" | socat - UNIX-CONNECT:"$socket_path" 2>/dev/null || true
elif command -v nc >/dev/null 2>&1; then
  printf '%s\n' "$json" | nc -U "$socket_path" 2>/dev/null || true
fi

exit 0
