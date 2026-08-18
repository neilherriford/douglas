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
#   ./run.sh                              # against $DOUGLAS_SMOKE_VM or "dev@douglas-dev.local"
#   DOUGLAS_SMOKE_VM=my-host ./run.sh
#   DOUGLAS_SMOKE_SSH_KEY=/path/to/key ./run.sh
#   DOUGLAS_SMOKE_SKIP_REBOOT=1 ./run.sh  # reuse the VM's current state as-is
#   ./steps/20-seedling-new.sh            # run a single step while iterating
set -uo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")"

ORANGE=$'\033[38;5;208m'
RESET=$'\033[0m'

for step in steps/[0-9]*.sh; do
    echo "${ORANGE}=== $step ===${RESET}"
    if ! bash "$step"; then
        echo "${ORANGE}----${RESET}"
        echo "FAILED at $step — stopping (later steps assume this one passed)"
        exit 1
    fi
done

echo "${ORANGE}----${RESET}"
echo "all steps passed"
