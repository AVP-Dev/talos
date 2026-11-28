#!/usr/bin/env bash
set -euo pipefail

# Run under xvfb-run on a clean Linux runner with the official CLI binaries.
# No account sign-in, credentials or model prompts are used in this capture.
binary="$(realpath "$1")"
output="$(realpath -m "$2")"
mkdir -p "$output"
scratch="$(mktemp -d)"
export TMUX_TMPDIR="$scratch"
unset TMUX TMUX_PANE
terminal_pid=''
cleanup() {
  tmux kill-server >/dev/null 2>&1 || true
  if [[ -n "$terminal_pid" ]]; then kill "$terminal_pid" 2>/dev/null || true; fi
  rm -rf -- "$scratch"
}
trap cleanup EXIT

mkdir -p "$scratch/demo-project"
command -v codex >/dev/null
command -v claude >/dev/null
test -z "${OPENAI_API_KEY:-}${ANTHROPIC_API_KEY:-}${ANTHROPIC_AUTH_TOKEN:-}"
cd "$scratch/demo-project"
git init -q
git -c user.name=Demo -c user.email=demo@example.invalid commit --allow-empty -qm 'Initial demo commit'
"$binary" config set writer codex
"$binary" config set critic claude

title='TALOS native workspace capture'
xterm -title "$title" -geometry 120x36 -fa 'DejaVu Sans Mono' -fs 13 -bg '#10141b' -fg '#dce6f1' -e "$binary" attach > "$scratch/xterm.log" 2>&1 &
terminal_pid=$!

wait_for() {
  for _ in $(seq 1 40); do
    if "$@"; then return 0; fi
    sleep 0.25
  done
  printf 'Timed out waiting for %s\n' "$1" >&2
  return 1
}
has_workspace() { tmux list-sessions -F '#{session_name}' 2>/dev/null | grep -q '^talos-pair-'; }
wait_for has_workspace
session="$(tmux list-sessions -F '#{session_name}' | head -n1)"
has_panes() {
  [[ $(tmux list-panes -t "$session" 2>/dev/null | wc -l) -eq 3 ]] &&
    tmux show-options -qv -t "$session" @talos_pair_panes 2>/dev/null | grep -Eq '^%[0-9]+ %[0-9]+ %[0-9]+$'
}
wait_for has_panes
read -r writer critic status < <(tmux show-options -qv -t "$session" @talos_pair_panes)
has_status() { tmux capture-pane -p -t "$status" | grep -q 'Native Pair'; }
wait_for has_status
native_ready() {
  tmux capture-pane -p -t "$writer" | grep -Eiq 'codex|openai|sign in' &&
    tmux capture-pane -p -t "$critic" | grep -Eiq 'claude|anthropic|welcome|theme'
}
wait_for native_ready
window="$(xdotool search --name "$title" | head -n1)"
sleep 1
import -window "$window" "$output/native-workspace.png"

tmux select-pane -t "$status"
xdotool windowfocus "$window"
xdotool key F2
has_settings() { tmux list-windows -t "$session" -F '#{window_name}' | grep -qx talos-settings; }
wait_for has_settings
settings_pane="$(tmux list-panes -t "$session:talos-settings" -F '#{pane_id}' | head -n1)"
settings_ready() { tmux capture-pane -p -t "$settings_pane" | grep -q 'TALOS Settings'; }
wait_for settings_ready
sleep 1
import -window "$window" "$output/native-settings.png"
test -s "$output/native-workspace.png"
test -s "$output/native-settings.png"
