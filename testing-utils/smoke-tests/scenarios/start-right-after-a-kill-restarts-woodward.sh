#!/usr/bin/env bash
# Regression: woodward's liveness is a heartbeat file with a 15s max age, so
# a woodward that was just killed still looked alive to `start` until that
# window passed, and `start` exited 0 without bringing it back. Liveness now
# also requires the heartbeat's pid to be alive.
set -uo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"
source ../lib.sh

run_prelude ../steps/00-reboot.sh ../steps/05-build.sh ../steps/10-start.sh

section "scenario: start right after a kill restarts woodward"

HEARTBEAT="/run/douglas/woodward-heartbeat/heartbeat"

original_pid="$(ssh_out "pgrep -f '[s]ervice woodward'")"
assert_non_empty "woodward has a pid before the kill" "$original_pid"

assert_success "kill woodward" ssh_out "sudo kill $original_pid"

heartbeat_age="$(ssh_out "echo \$(( \$(date +%s) - \$(sudo stat -c %Y '$HEARTBEAT') ))")"
if [ -n "$heartbeat_age" ] && [ "$heartbeat_age" -lt 15 ]; then
    pass "the dead woodward's heartbeat is still fresh (${heartbeat_age}s old), the case that used to fool start"
else
    fail "the dead woodward's heartbeat should still be fresh (got '${heartbeat_age}')"
    FAILURES=$((FAILURES + 1))
fi

assert_success "douglas start exits successfully" ssh_out \
    "sudo ~/douglas --output-style plain start"

woodward_running() {
    ssh_out "pgrep -f '[s]ervice woodward'" >/dev/null 2>&1
}
wait_until "start brought woodward back" 10 woodward_running

new_pid="$(ssh_out "pgrep -f '[s]ervice woodward'")"
assert_not_equals "woodward restarted under a new pid" "$original_pid" "$new_pid"

heartbeat_contents="$(ssh_out "sudo cat '$HEARTBEAT'")"
assert_contains "the new woodward is writing the heartbeat" "$heartbeat_contents" "\"pid\":$new_pid"

finish
