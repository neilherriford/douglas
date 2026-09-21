#!/usr/bin/env bash
# Runs every smoke test step in steps/, in lexical (numeric-prefix) order,
# against the douglas dev VM (happy path only).
#
# Each step declares which douglas CLI command(s) it exercises via a
# `# covers: <command path>` comment near the top of the file — that's
# what `command_coverage_tests` in src/main.rs cross-checks against the
# CLI's actual command surface, so a command with no covering step fails
# `cargo test` loudly instead of the smoke suite silently rotting.
#
# This is a linear happy-path lifecycle, not an independent test suite —
# each step assumes every prior step succeeded (e.g. there's no seedling to
# check the status of if `seedling new` never ran). So a failing step stops
# the run immediately instead of cascading into a wall of misleading
# failures from steps that never had a chance to pass.
#
# The first step reboots the VM back to the live-CD's pristine state before
# anything else runs (set DOUGLAS_SMOKE_SKIP_REBOOT=1 to skip that).
#
# Usage:
#   ./happy-path.sh                              # against $DOUGLAS_SMOKE_VM or "dev@douglas-dev.local"
#   DOUGLAS_SMOKE_VM=my-host ./happy-path.sh
#   DOUGLAS_SMOKE_SSH_KEY=/path/to/key ./happy-path.sh
#   DOUGLAS_SMOKE_SKIP_REBOOT=1 ./happy-path.sh  # reuse the VM's current state as-is
#   ./steps/20-seedling-new.sh                   # run a single step while iterating
#   ./happy-path.sh --only 81,82,86              # prerequisites (00, 05, 06, 10) + just these step numbers
#   ./happy-path.sh --from 81 --to 87            # prerequisites + every step numbered 81 through 87
set -uo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")"
source lib.sh

PREREQUISITES=(00 05 06 10)

usage() {
    echo "usage: $0 [--only N[,N...]] [--from N] [--to N]" >&2
    exit 2
}

only=""
from=""
to=""
while [ $# -gt 0 ]; do
    case "$1" in
        --only) only="${2:-}"; shift 2 || usage ;;
        --from) from="${2:-}"; shift 2 || usage ;;
        --to) to="${2:-}"; shift 2 || usage ;;
        *) usage ;;
    esac
done

selected() {
    local number="$1" wanted
    if [ -z "$only$from$to" ]; then
        return 0
    fi
    for wanted in "${PREREQUISITES[@]}"; do
        [ "$number" = "$wanted" ] && return 0
    done
    if [ -n "$only" ]; then
        for wanted in ${only//,/ }; do
            [ "$number" = "$wanted" ] && return 0
        done
        return 1
    fi
    [ -z "$from" ] || [ "$((10#$number))" -ge "$((10#$from))" ] || return 1
    [ -z "$to" ] || [ "$((10#$number))" -le "$((10#$to))" ] || return 1
}

run_started_at=$SECONDS

for step in steps/[0-9]*.sh; do
    step_number="$(basename "$step" | cut -d- -f1)"
    selected "$step_number" || continue
    step_started_at=$SECONDS
    echo "${ORANGE}=== $step (started $(date '+%H:%M:%S')) ===${RESET}"
    if ! bash "$step"; then
        step_elapsed=$((SECONDS - step_started_at))
        echo "${ORANGE}----${RESET}"
        echo "FAILED at $step after ${step_elapsed}s — stopping (later steps assume this one passed)"
        exit 1
    fi
    step_elapsed=$((SECONDS - step_started_at))
    echo "${ORANGE}--- $step finished in ${step_elapsed}s ---${RESET}"
done

total_elapsed=$((SECONDS - run_started_at))
echo "${ORANGE}----${RESET}"
echo "all steps passed in ${total_elapsed}s"
