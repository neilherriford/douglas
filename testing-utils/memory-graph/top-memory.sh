#!/usr/bin/env bash
set -uo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"
source ../smoke-tests/lib.sh

INTERVAL_SECONDS=2
MAX_SAMPLES=0
SCALE_MODE=range
HISTORY_TICKS=600
COLOR=1
LOCAL=0
TARGET="$VM"
TOP_CONTROL_PATH="/tmp/douglas-top-%C"
SSH_OPTS=(
    -o LogLevel=ERROR
    -o ConnectTimeout=5
    -o BatchMode=yes
    -o ControlMaster=auto
    -o ControlPath="$TOP_CONTROL_PATH"
    -o ControlPersist=60s
    -i "$SSH_KEY"
)
WORK_DIR=""
HISTORY_FILE=""
STATUS="starting"
FAILED=0
SAMPLE_SECONDS=0
ALT_SCREEN=0
COLS=100
ROWS=999

read -r -d '' REMOTE_SAMPLE <<'EOF'
ps -eo rss=,args= | awk '/douglas/ && match($0, / service [a-z][a-z0-9_-]*/) {
    name = substr($0, RSTART + 9, RLENGTH - 9)
    if (!(name in seen)) { seen[name] = 1; print "P " name " " $1 }
}'
docker stats --no-stream --format 'C {{.Name}} {{.MemUsage}}' 2>/dev/null | grep '^C doug'
exit 0
EOF

usage() {
    cat <<EOF
Usage: top-memory.sh [-l] [-i SECONDS] [-n SAMPLES] [-s zero|range|global]

Live console graph of memory use for the douglas processes and containers
running on the dev VM, one sparkline per process/container plus a total.
Runs over ssh from your machine, or directly when run on the VM itself.

  -l           sample this machine directly instead of over ssh
                 (automatic when the hostname matches the VM)
  -i SECONDS   sampling interval (default $INTERVAL_SECONDS)
  -n SAMPLES   take SAMPLES samples, print one frame, and exit
  -s MODE      sparkline scale (default $SCALE_MODE)
                 range   each row spans its own min to max
                 zero    each row spans 0 to its own max
                 global  every row spans 0 to the largest row
  -h           show this help

Keys: q quit, s cycle the scale mode.
Environment: DOUGLAS_SMOKE_VM, DOUGLAS_SMOKE_SSH_KEY, NO_COLOR.
EOF
}

parse_options() {
    local option
    while getopts "li:n:s:h" option; do
        case "$option" in
        l) LOCAL=1 ;;
        i) INTERVAL_SECONDS="$OPTARG" ;;
        n) MAX_SAMPLES="$OPTARG" ;;
        s) SCALE_MODE="$OPTARG" ;;
        h) usage; exit 0 ;;
        *) usage >&2; exit 2 ;;
        esac
    done
    [[ "$INTERVAL_SECONDS" =~ ^[1-9][0-9]*$ ]] || { echo "-i must be a positive integer" >&2; exit 2; }
    [[ "$MAX_SAMPLES" =~ ^[0-9]+$ ]] || { echo "-n must be a non-negative integer" >&2; exit 2; }
    [[ "$SCALE_MODE" =~ ^(zero|range|global)$ ]] || { echo "-s must be zero, range, or global" >&2; exit 2; }
}

cycle_scale() {
    case "$SCALE_MODE" in
    range) SCALE_MODE=zero ;;
    zero) SCALE_MODE=global ;;
    *) SCALE_MODE=range ;;
    esac
}

detect_local() {
    local host="${VM#*@}"
    host="${host%.local}"
    [ "$(hostname -s 2>/dev/null)" = "$host" ] && LOCAL=1
    [ "$LOCAL" -eq 1 ] && TARGET="local ($(hostname -s 2>/dev/null))"
    return 0
}

enter_screen() {
    if [ -t 1 ] && [ "$MAX_SAMPLES" -eq 0 ]; then
        tput smcup
        tput civis
        ALT_SCREEN=1
    fi
    if [ ! -t 1 ] || [ -n "${NO_COLOR:-}" ]; then
        COLOR=0
    fi
}

cleanup() {
    if [ "$ALT_SCREEN" -eq 1 ]; then
        tput cnorm
        tput rmcup
    fi
    [ "$LOCAL" -eq 0 ] && ssh "${SSH_OPTS[@]}" -O exit "$VM" >/dev/null 2>&1
    [ -n "$WORK_DIR" ] && rm -rf "$WORK_DIR"
}

run_sampler() {
    if [ "$LOCAL" -eq 1 ]; then
        bash -s <<<"$REMOTE_SAMPLE"
    else
        ssh "${SSH_OPTS[@]}" "$VM" bash -s <<<"$REMOTE_SAMPLE"
    fi
}

sample() {
    run_sampler 2>"$WORK_DIR/sample.err" | grep -vE '^(🚀|📁|📦|🔧)|^$'
    return "${PIPESTATUS[0]}"
}

prune_history() {
    local oldest=$(($1 - HISTORY_TICKS))
    awk -F, -v oldest="$oldest" '$1 > oldest' "$HISTORY_FILE" >"$HISTORY_FILE.tmp" && mv "$HISTORY_FILE.tmp" "$HISTORY_FILE"
}

take_sample() {
    local tick="$1" started="$SECONDS" raw code
    raw="$(sample)"
    code=$?
    SAMPLE_SECONDS=$((SECONDS - started))
    if [ "$code" -eq 0 ]; then
        printf '%s\n' "$raw" | awk -v tick="$tick" -f top-sample.awk >>"$HISTORY_FILE"
        STATUS="last sample $(date +%H:%M:%S)"
        FAILED=0
    else
        STATUS="sample failed (exit $code: $(head -1 "$WORK_DIR/sample.err" | cut -c1-80)) at $(date +%H:%M:%S)"
        FAILED=1
    fi
    [ $((tick % 50)) -eq 0 ] && prune_history "$tick"
}

measure_terminal() {
    if [ -t 1 ]; then
        COLS="$(tput cols 2>/dev/null || echo 100)"
        ROWS="$(tput lines 2>/dev/null || echo 40)"
    fi
}

render() {
    awk -v cols="$COLS" -v lines="$ROWS" -v mode="$SCALE_MODE" -v status="$STATUS" \
        -v failed="$FAILED" -v vm="$TARGET" -v interval="$INTERVAL_SECONDS" -v color="$COLOR" \
        -f top-render.awk "$HISTORY_FILE"
}

draw() {
    measure_terminal
    [ "$ALT_SCREEN" -eq 1 ] && tput cup 0 0
    render
    [ "$ALT_SCREEN" -eq 1 ] && tput ed
    return 0
}

pause_between_samples() {
    local remaining=$((INTERVAL_SECONDS - SAMPLE_SECONDS)) deadline key
    [ "$remaining" -lt 1 ] && remaining=1
    deadline=$((SECONDS + remaining))
    while [ "$SECONDS" -lt "$deadline" ]; do
        key=""
        if [ -t 0 ]; then
            read -rsn1 -t 1 key
        else
            sleep 1
        fi
        case "$key" in
        q | Q) return 1 ;;
        s | S)
            cycle_scale
            draw
            ;;
        esac
    done
    return 0
}

main() {
    local tick=0
    while :; do
        tick=$((tick + 1))
        take_sample "$tick"
        if [ "$MAX_SAMPLES" -eq 0 ]; then
            draw
        elif [ "$tick" -ge "$MAX_SAMPLES" ]; then
            draw
            break
        fi
        pause_between_samples || break
    done
}

parse_options "$@"
WORK_DIR="$(mktemp -d)"
HISTORY_FILE="$WORK_DIR/history.csv"
: >"$HISTORY_FILE"
trap cleanup EXIT
trap 'exit 130' INT TERM
detect_local
enter_screen
main
