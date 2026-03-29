#!/bin/bash
# grepvec UI toggle — Shift+F9
# If already running: toggle visibility (instant)
# If not running: only launch if focused window is a terminal in a grepvec project

# Always toggle if grepvec-ui is already running
if hyprctl clients -j 2>/dev/null | jq -e '.[] | select(.title == "grepvec")' >/dev/null 2>&1; then
    hyprctl dispatch togglespecialworkspace grepvec
    exit 0
fi

# Not running — check if we're in a grepvec-configured terminal
# Get the PID of the focused window's process
FOCUSED_PID=$(hyprctl activewindow -j 2>/dev/null | jq -r '.pid // empty')
if [ -z "$FOCUSED_PID" ]; then
    exit 0
fi

# Get the cwd of the focused process (or its child shell)
# Terminals typically have a child shell process — check both
CWD=""
for pid in "$FOCUSED_PID" $(pgrep -P "$FOCUSED_PID" 2>/dev/null | head -5); do
    candidate=$(readlink "/proc/$pid/cwd" 2>/dev/null)
    if [ -n "$candidate" ]; then
        # Walk up from cwd looking for .grepvec/scope.toml
        dir="$candidate"
        while [ "$dir" != "/" ]; do
            if [ -f "$dir/.grepvec/scope.toml" ]; then
                CWD="$dir"
                break 2
            fi
            dir=$(dirname "$dir")
        done
    fi
done

if [ -z "$CWD" ]; then
    # Not in a grepvec project — do nothing
    exit 0
fi

# In a grepvec project — launch grepvec-ui with auto-loaded credentials
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
nohup "$SCRIPT_DIR/target/release/grepvec-ui" >/dev/null 2>&1 &
disown

# Wait for window to appear, then show the special workspace
for i in $(seq 1 50); do
    sleep 0.3
    if hyprctl clients -j 2>/dev/null | jq -e '.[] | select(.title == "grepvec")' >/dev/null 2>&1; then
        hyprctl dispatch togglespecialworkspace grepvec
        exit 0
    fi
done
