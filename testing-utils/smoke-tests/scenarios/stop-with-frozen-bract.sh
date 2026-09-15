#!/usr/bin/env bash
# Exercises the escalation path in `StopBract` (src/bootstrap/stop.rs) that
# the happy-path `90-stop.sh` step can't reach: bract frozen with SIGSTOP
# can't respond to the `stop` UDS request at all, so `douglas stop` has to
# time out on it (BRACT_STOP_TIMEOUT), fall back to stopping every
# "doug."/"doug-agent."-prefixed container directly via Docker, and finally
# force-kill bract's pid — instead of the clean bract-acknowledges path.
set -uo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"
source ../lib.sh

run_prelude ../steps/00-reboot.sh ../steps/05-build.sh ../steps/10-start.sh

section "scenario: stop escalates when bract is frozen"

DOUGLAS_CLI_LOG="/var/log/douglas/douglas-cli/douglas-cli.log"
BRACT_STOP_TIMEOUT_SECONDS=30

bract_pid="$(ssh_out "pgrep -f '[s]ervice bract'")"
assert_success "bract has a pid before the freeze" test -n "$bract_pid"

douglas_cli_log_lines_before="$(log_line_count "$DOUGLAS_CLI_LOG")"

assert_success "freeze bract so it can't respond to the stop request" ssh_out \
    "sudo kill -STOP $bract_pid"

stop_started_at=$SECONDS
assert_success "douglas stop escalates past the frozen bract" ssh_out \
    "sudo ~/douglas --output-style plain stop"
stop_elapsed=$((SECONDS - stop_started_at))

assert_success "stop actually waited out the bract timeout, not a fast-path skip" \
    test "$stop_elapsed" -ge "$BRACT_STOP_TIMEOUT_SECONDS"

stop_log="$(ssh_out sudo tail -n "+$((douglas_cli_log_lines_before + 1))" "$DOUGLAS_CLI_LOG")"
assert_contains "logged the bract stop request timing out" "$stop_log" \
    "Bract stop request timed out"
assert_contains "logged falling back to stopping a douglas container directly" "$stop_log" \
    "Stopping container doug."

bract_still_alive() { ssh_out "sudo kill -0 $bract_pid" >/dev/null 2>&1; }
assert_failure "the frozen bract process was force-killed" bract_still_alive

for service in woodward seedbank resin; do
    assert_failure "$service is no longer running" \
        ssh_out "pgrep -f '[s]ervice $service'"
done

assert_success "no douglas-managed containers are still running" ssh_out \
    "! docker ps --filter name=doug --filter status=running -q | grep -q ."

finish
