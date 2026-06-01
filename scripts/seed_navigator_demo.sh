#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
usage: scripts/seed_navigator_demo.sh [--allow-main]

Seeds a running herdr server with navigator demo workspaces, tabs, panes,
and fake agent states for recording the session navigator.

Environment:
  HERDR_NAV_SOCKET_PATH  API socket to target. Defaults to $HOME/.config/herdr-dev/herdr.sock.
  HERDR_NAV_CWD          Workspace cwd for created panes. Defaults to the repo root.
  HERDR_NAV_BIN          Herdr binary to call. Defaults to cargo run from the repo.
USAGE
}

allow_main=0
while (($#)); do
  case "$1" in
    --allow-main)
      allow_main=1
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "unknown option: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo_dir="$(cd -- "$script_dir/.." && pwd)"
workspace_cwd="${HERDR_NAV_CWD:-$repo_dir}"
config_home="${XDG_CONFIG_HOME:-$HOME/.config}"
dev_socket="$config_home/herdr-dev/herdr.sock"
main_socket="$config_home/herdr/herdr.sock"
export HERDR_SOCKET_PATH="${HERDR_NAV_SOCKET_PATH:-$dev_socket}"

if [[ "$allow_main" != 1 && "$HERDR_SOCKET_PATH" == "$main_socket" ]]; then
  echo "refusing to seed main herdr session: $HERDR_SOCKET_PATH" >&2
  echo "use HERDR_NAV_SOCKET_PATH for a dev socket, or pass --allow-main intentionally" >&2
  exit 1
fi

if [[ ! -S "$HERDR_SOCKET_PATH" ]]; then
  echo "herdr socket not found: $HERDR_SOCKET_PATH" >&2
  echo "start a dev server first, or set HERDR_NAV_SOCKET_PATH" >&2
  exit 1
fi

cd "$repo_dir"

if [[ -n "${HERDR_NAV_BIN:-}" ]]; then
  run() { "$HERDR_NAV_BIN" "$@"; }
else
  run() { cargo run --quiet -- "$@"; }
fi

mkws() {
  local label="$1"
  run workspace create --label "$label" --cwd "$workspace_cwd" --no-focus \
    | jq -r '.result.workspace.workspace_id + " " + .result.root_pane.pane_id + " " + .result.tab.tab_id'
}

mktab() {
  local ws="$1" label="$2"
  run tab create --workspace "$ws" --label "$label" --cwd "$workspace_cwd" --no-focus \
    | jq -r '.result.tab.tab_id + " " + .result.root_pane.pane_id'
}

split() {
  local pane="$1" direction="$2"
  run pane split "$pane" --direction "$direction" --no-focus \
    | jq -r '.result.pane.pane_id'
}

rename_sparse() {
  local pane="$1" label="$2"
  run pane rename "$pane" "$label" >/dev/null
}

report() {
  local pane="$1" agent="$2" state="$3" status="$4" seq="$5"
  run pane report-agent "$pane" \
    --source nav-seed \
    --agent "$agent" \
    --state "$state" \
    --custom-status "$status" \
    --seq "$seq" >/dev/null
}

done_panes=()

read API_WS API_CODEX API_TAB < <(mkws api)
run tab rename "$API_TAB" agents >/dev/null
API_SHELL="$(split "$API_CODEX" down)"
API_CLAUDE="$(split "$API_CODEX" right)"
read API_LOGS_TAB API_LOGS < <(mktab "$API_WS" logs)
rename_sparse "$API_CODEX" "codex env review"
rename_sparse "$API_CLAUDE" "claude api refactor"
rename_sparse "$API_SHELL" "api shell"
rename_sparse "$API_LOGS" "deploy logs"
report "$API_CODEX" codex blocked "env approval" 1
report "$API_CLAUDE" claude working "refactor api" 1
report "$API_SHELL" shell idle ready 1
report "$API_LOGS" deploy working "tail logs" 1

read WEB_WS WEB_PI WEB_TAB < <(mkws web)
run tab rename "$WEB_TAB" agents >/dev/null
WEB_CLAUDE="$(split "$WEB_PI" down)"
WEB_CODEX="$(split "$WEB_PI" right)"
read WEB_PREVIEW_TAB WEB_PREVIEW < <(mktab "$WEB_WS" preview)
rename_sparse "$WEB_PI" "pi ui build"
rename_sparse "$WEB_CODEX" "codex css review"
rename_sparse "$WEB_CLAUDE" "claude approval"
rename_sparse "$WEB_PREVIEW" "preview server"
report "$WEB_PI" pi working "build ui" 1
report "$WEB_CLAUDE" claude blocked "tool approval" 1
report "$WEB_PREVIEW" server idle "preview ready" 1
report "$WEB_CODEX" codex working "css pass" 1
done_panes+=("$WEB_CODEX:codex:review ready")

read DOCS_WS DOCS_NOTES DOCS_TAB < <(mkws docs)
run tab rename "$DOCS_TAB" release >/dev/null
DOCS_COPY="$(split "$DOCS_NOTES" right)"
rename_sparse "$DOCS_NOTES" "codex notes done"
rename_sparse "$DOCS_COPY" "codex release copy"
report "$DOCS_COPY" codex working "release copy" 1
report "$DOCS_NOTES" codex working "checking notes" 1
done_panes+=("$DOCS_NOTES:codex:notes done")

read INFRA_WS INFRA_HERMES INFRA_TAB < <(mkws infra)
run tab rename "$INFRA_TAB" ops >/dev/null
INFRA_SSH="$(split "$INFRA_HERMES" right)"
rename_sparse "$INFRA_HERMES" "hermes ssh prompt"
rename_sparse "$INFRA_SSH" "ssh monitor"
report "$INFRA_HERMES" hermes blocked "ssh prompt" 1
report "$INFRA_SSH" ssh idle connected 1

run workspace focus "$API_WS" >/dev/null
seq=2
for item in "${done_panes[@]}"; do
  pane="${item%%:*}"
  rest="${item#*:}"
  agent="${rest%%:*}"
  status="${rest#*:}"
  report "$pane" "$agent" idle "$status" "$seq"
  seq=$((seq + 1))
done

cat <<EOF
Seeded navigator demo data via $HERDR_SOCKET_PATH

Workspaces:
  $API_WS api     agents: blocked codex, working claude, idle shell; logs: working deploy
  $WEB_WS web     agents: working pi, done codex, blocked claude; preview: idle server
  $DOCS_WS docs   release: done codex, working codex
  $INFRA_WS infra ops: blocked hermes, idle ssh

Test in navigator: / then b, w, i, d, a, or normal text.
EOF
