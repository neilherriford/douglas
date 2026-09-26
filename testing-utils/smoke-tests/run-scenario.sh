#!/usr/bin/env bash
# Runs one scenario against the VM named by DOUGLAS_SMOKE_VM: every script in
# setup/ (deploy the binary, `douglas start`), then every script in
# scenarios/<name>/ in lexical order. Steps inside a scenario share state and
# build on each other, so the first failing step stops the run instead of
# cascading into misleading failures from steps that never had a chance.
#
# ci-fanout.sh calls this once per scenario, each on its own fresh VM. When a
# scenario fails its VM is left running for debugging; to iterate on it by
# hand against that VM (its ssh target is printed by ci-fanout.sh):
#
#   DOUGLAS_SMOKE_VM=... DOUGLAS_SMOKE_SSH_KEY=... ./run-scenario.sh upgrade-rollback --no-setup --only 85,86
#
# For the audit trail, ci-fanout.sh sets these (all optional):
#   DOUGLAS_SMOKE_STEP_LOG      TSV, one row per step: step status seconds passes fails
#   DOUGLAS_SMOKE_FAILURES_LOG  TSV, one row per failed check: step<TAB>text
#   DOUGLAS_SMOKE_PROGRESS_FILE shared progress feed the operator sees live
#   DOUGLAS_SMOKE_NOW_FILE      single line: what this scenario is doing right now
#   DOUGLAS_SMOKE_LANE          the scenario's name, for the progress feed
#
# Usage: run-scenario.sh <scenario> [--no-setup] [--only NN[,NN...]]
set -uo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"

usage() {
    echo "usage: $0 <scenario> [--no-setup] [--only NN[,NN...]]" >&2
    exit 2
}

scenario="${1:-}"
[ -n "$scenario" ] || usage
shift
[ -d "scenarios/$scenario" ] || { echo "no such scenario: scenarios/$scenario" >&2; exit 2; }

run_setup=1
only=""
while [ $# -gt 0 ]; do
    case "$1" in
        --no-setup) run_setup=0; shift ;;
        --only) only="${2:-}"; shift 2 || usage ;;
        *) usage ;;
    esac
done

selected() {
    local number wanted
    number="$(basename "$1" | cut -d- -f1)"
    [ -z "$only" ] && return 0
    for wanted in ${only//,/ }; do
        [ "$number" = "$wanted" ] && return 0
    done
    return 1
}

strip_colors() {
    sed $'s/\x1b\\[[0-9;]*m//g'
}

progress() {
    [ -n "${DOUGLAS_SMOKE_PROGRESS_FILE:-}" ] || return 0
    printf '%s\t%s\t%s\t%s\n' "$(date +%s)" "${DOUGLAS_SMOKE_LANE:-$scenario}" "$1" "$2" \
        >>"$DOUGLAS_SMOKE_PROGRESS_FILE"
}

now() {
    [ -n "${DOUGLAS_SMOKE_NOW_FILE:-}" ] || return 0
    echo "$1" >"$DOUGLAS_SMOKE_NOW_FILE"
}

run_started_at=$SECONDS
total_passes=0
total_fails=0
steps_run=0

run_step() {
    local step="$1" started_at=$SECONDS output status passes fails elapsed result
    output="$(mktemp)"
    echo "=== $step (started $(date '+%H:%M:%S')) ==="
    now "running $step"
    bash "$step" 2>&1 | tee "$output"
    status="${PIPESTATUS[0]}"
    elapsed=$((SECONDS - started_at))
    passes="$(strip_colors <"$output" | grep -c '^  PASS: ')"
    fails="$(strip_colors <"$output" | grep -c '^  FAIL: ')"
    result="PASS"
    [ "$status" -eq 0 ] || result="FAIL"

    total_passes=$((total_passes + passes))
    total_fails=$((total_fails + fails))
    steps_run=$((steps_run + 1))

    [ -z "${DOUGLAS_SMOKE_STEP_LOG:-}" ] || \
        printf '%s\t%s\t%s\t%s\t%s\n' "$step" "$result" "$elapsed" "$passes" "$fails" >>"$DOUGLAS_SMOKE_STEP_LOG"
    if [ -n "${DOUGLAS_SMOKE_FAILURES_LOG:-}" ]; then
        strip_colors <"$output" | grep '^  FAIL: ' | sed 's/^  FAIL: //' | \
            while IFS= read -r line; do printf '%s\t%s\n' "$step" "$line"; done >>"$DOUGLAS_SMOKE_FAILURES_LOG"
    fi
    progress step "$step $result ${elapsed}s $passes checks passed, $fails failed"
    rm -f "$output"

    if [ "$status" -ne 0 ]; then
        echo "----"
        echo "FAILED at $step after ${elapsed}s — stopping (later steps assume this one passed)"
        exit 1
    fi
    echo "--- $step finished in ${elapsed}s ($passes checks) ---"
}

if [ "$run_setup" -eq 1 ]; then
    for step in setup/[0-9]*.sh; do
        run_step "$step"
    done
fi

for step in scenarios/"$scenario"/*.sh; do
    selected "$step" || continue
    run_step "$step"
done

now "finished"
echo "----"
echo "scenario $scenario passed in $((SECONDS - run_started_at))s ($steps_run steps, $total_passes checks)"
