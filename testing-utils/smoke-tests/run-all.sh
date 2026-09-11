#!/usr/bin/env bash
set -uo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")"

BOLD_CYAN=$'\033[1;36m'
RESET=$'\033[0m'
BAR="################################################################"

run_started_at=$SECONDS

run_labels=()
run_elapsed=()
run_ok=()

run_one() {
    local label="$1" script="$2"
    local started_at=$SECONDS

    echo
    echo "${BOLD_CYAN}${BAR}${RESET}"
    echo "${BOLD_CYAN}# $label (started $(date '+%H:%M:%S'))${RESET}"
    echo "${BOLD_CYAN}${BAR}${RESET}"
    echo

    bash "$script" 2>&1 | sed 's/^/    /'
    local status="${PIPESTATUS[0]}"
    local elapsed=$((SECONDS - started_at))

    echo
    run_labels+=("$label")
    run_elapsed+=("$elapsed")
    if [ "$status" -eq 0 ]; then
        run_ok+=(1)
        echo "${BOLD_CYAN}# $label — PASSED in ${elapsed}s${RESET}"
        echo "${BOLD_CYAN}${BAR}${RESET}"
        return 0
    fi

    run_ok+=(0)
    echo "${BOLD_CYAN}# $label — FAILED after ${elapsed}s${RESET}"
    echo "${BOLD_CYAN}${BAR}${RESET}"
    return 1
}

print_summary() {
    local total_elapsed=$((SECONDS - run_started_at))
    echo
    echo "${BOLD_CYAN}${BAR}${RESET}"
    echo "${BOLD_CYAN}# summary (${total_elapsed}s total)${RESET}"
    for index in "${!run_labels[@]}"; do
        if [ "${run_ok[$index]}" -eq 1 ]; then
            echo "${BOLD_CYAN}#   passed  ${run_labels[$index]} — ${run_elapsed[$index]}s${RESET}"
        else
            echo "${BOLD_CYAN}#   FAILED  ${run_labels[$index]} — ${run_elapsed[$index]}s${RESET}"
        fi
    done
    echo "${BOLD_CYAN}${BAR}${RESET}"
}

abort_after_failure() {
    print_summary
    echo
    echo "${BOLD_CYAN}${BAR}${RESET}"
    echo "${BOLD_CYAN}# stopping here — the VM is left as-is (not rebooting for the next run)${RESET}"
    echo "${BOLD_CYAN}# so its live state is still there to debug the failure above${RESET}"
    echo "${BOLD_CYAN}${BAR}${RESET}"
    exit 1
}

run_one "happy path (happy-path.sh)" "happy-path.sh" || abort_after_failure

for scenario in scenarios/*.sh; do
    run_one "scenario: $(basename "$scenario" .sh)" "$scenario" || abort_after_failure
done

print_summary
exit 0
