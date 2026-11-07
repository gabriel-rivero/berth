#!/usr/bin/env sh
# Lightweight-budget probe: binary size, first-frame time, idle RSS.
# Usage: scripts/lightweight.sh [path-to-berth]   (default target/release/berth)
set -eu

BIN=${1:-target/release/berth}
if [ ! -x "$BIN" ]; then
    echo "missing $BIN - run: cargo build --release -p berth" >&2
    exit 1
fi

bytes=$(wc -c <"$BIN" | tr -d ' ')
echo "binary:   $BIN"
echo "size:     $bytes bytes ($(du -h "$BIN" | cut -f1))"

if ! command -v Xvfb >/dev/null 2>&1; then
    echo "startup:  skipped (Xvfb not installed)"
    exit 0
fi

cfg=$(mktemp -d)
display=":${BERTH_BUDGET_DISPLAY:-91}"
Xvfb "$display" -screen 0 1024x768x24 >/dev/null 2>&1 &
xvfb_pid=$!
app_pid=
cleanup() {
    [ -n "$app_pid" ] && kill "$app_pid" 2>/dev/null || true
    kill "$xvfb_pid" 2>/dev/null || true
    rm -rf "$cfg"
}
trap cleanup EXIT INT TERM

sleep 0.5
start=$(date +%s%N)
DISPLAY=$display XDG_CONFIG_HOME=$cfg BERTH_DEBUG=1 "$BIN" >/dev/null 2>&1 &
app_pid=$!
log="$cfg/berth/berth.log"

# First rendered frame is the meaningful "ready" point (GPU init + shell spawn).
i=0
while [ "$i" -lt 600 ]; do
    if grep -q "draw done" "$log" 2>/dev/null; then
        break
    fi
    if ! kill -0 "$app_pid" 2>/dev/null; then
        echo "startup:  FAILED (process exited before first frame)" >&2
        exit 1
    fi
    sleep 0.02
    i=$((i + 1))
done
end=$(date +%s%N)
if grep -q "draw done" "$log" 2>/dev/null; then
    echo "startup:  $(( (end - start) / 1000000 )) ms to first frame"
else
    echo "startup:  FAILED (no frame within 12 s)" >&2
fi

# Idle RSS once the app has settled (blink timer + shell prompt).
sleep 3
if [ -r "/proc/$app_pid/status" ]; then
    rss=$(awk '/^VmRSS:/ {print $2}' "/proc/$app_pid/status")
    echo "idle RSS: $rss KiB"
else
    echo "idle RSS: unavailable (non-Linux)"
fi
