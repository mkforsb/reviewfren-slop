#!/bin/bash
# Test harness: runs the app on a fixture repo under Xvfb, with a fake `gh` and
# the mock Claude, so it can be driven with xdotool and screenshotted.
#
#   dev/harness.sh setup             create the fixture repo (remote + clone + PR branch)
#   dev/harness.sh start [binary]    start Xvfb and the app on the PR list (builds
#                                    the debug binary unless one is given)
#   dev/harness.sh open [binary]     start, open PR #7 and focus the window
#   dev/harness.sh shot [file] [WxH+X+Y]   screenshot (optionally cropped)
#   dev/harness.sh stop              stop the app and Xvfb
#
# Needs Xvfb, xdotool and ImageMagick (`import`); xclip is handy for checking the
# clipboard (`DISPLAY=:99 xclip -o -selection clipboard`). Files go to
# $HARNESS_DIR (default /tmp/pr-review-harness); the app's log is app.log there.
set -e
ROOT=$(cd "$(dirname "$0")/.." && pwd)
SRC=$ROOT/dev/harness
H=${HARNESS_DIR:-/tmp/pr-review-harness}
export DISPLAY=${HARNESS_DISPLAY:-:99}

# Kills the process whose pid is in file $1, if it is still running.
kill_pidfile() {
    if [ -f "$1" ]; then
        kill "$(cat "$1")" 2>/dev/null || true
        rm -f "$1"
    fi
}

setup() {
    kill_pidfile "$H/app.pid"
    rm -rf "$H"
    mkdir -p "$H/home/.config/reviewfren"
    git init -q --bare -b main "$H/remote.git"
    git clone -q "$H/remote.git" "$H/repo" 2>/dev/null
    cd "$H/repo"
    git config user.email harness@example.com
    git config user.name Harness
    cp -r "$SRC/fixture/base/." .
    git add -A && git commit -qm init && git push -q origin main 2>/dev/null
    git checkout -qb feature
    cp -r "$SRC/fixture/pr/." .
    git commit -qam "Rename Spans to SpanSet, highlight keywords" && git push -q origin feature 2>/dev/null
    git checkout -q main && git branch -qD feature
    cat > "$H/home/.config/reviewfren/settings.json" <<JSON
{"claude_command": "python3 $ROOT/dev/mock_claude.py {repo}"}
JSON
    echo "fixture ready in $H"
}

start() {
    [ -d "$H/repo" ] || setup
    if ! xdpyinfo >/dev/null 2>&1 && ! xdotool getmouselocation >/dev/null 2>&1; then
        setsid Xvfb "$DISPLAY" -screen 0 1600x1000x24 >/dev/null 2>&1 &
        echo $! >"$H/xvfb.pid"
        sleep 2
    fi
    kill_pidfile "$H/app.pid"
    sleep 0.3
    # The app checks out the PR branch; start from main, with the branch only on the remote.
    (cd "$H/repo" && git checkout -q main && { git branch -qD feature 2>/dev/null || true; })
    local bin=${1:-}
    if [ -z "$bin" ]; then
        cargo build --quiet --manifest-path "$ROOT/Cargo.toml"
        bin=$ROOT/target/debug/reviewfren
    fi
    HOME=$H/home XDG_DATA_HOME=$H/home/.local/share PATH=$SRC/bin:$PATH setsid "$bin" "$H/repo" >"$H/app.log" 2>&1 &
    echo $! >"$H/app.pid"
    sleep 3
}

open_pr() {
    start "$@"
    xdotool mousemove 345 160 click 1
    sleep 4
    # Without a window manager, keyboard focus has to be set explicitly.
    xdotool windowfocus --sync "$(xdotool search --name 'PR #7' | head -1)"
}

case "${1:-}" in
    setup) setup ;;
    start) start "${@:2}" ;;
    open) open_pr "${@:2}" ;;
    shot) import -window root ${3:+-crop "$3"} "${2:-$H/shot.png}" ;;
    stop) kill_pidfile "$H/app.pid"; kill_pidfile "$H/xvfb.pid" ;;
    *) sed -n '2,/^[^#]/{/^#/p}' "$0"; exit 1 ;;
esac
