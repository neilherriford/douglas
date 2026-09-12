#!/usr/bin/env bash
set -uo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"
source ../lib.sh

HEARTBEAT_PATH="/run/douglas/seedbank-heartbeat/heartbeat"
WOODWARD_LOG="/var/log/douglas/woodward/woodward.log"

original_pid="$(ssh_out "pgrep -f '[s]ervice seedbank'")"
assert_success "seedbank has a pid before the freeze" test -n "$original_pid"

woodward_log_lines_before="$(log_line_count "$WOODWARD_LOG")"

assert_success "freeze seedbank to simulate a hang" ssh_out \
    "sudo kill -STOP $original_pid"

woodward_kicked_it() {
    local current_pid
    current_pid="$(ssh_out "pgrep -f '[s]ervice seedbank'" 2>/dev/null)"
    [ -n "$current_pid" ] && [ "$current_pid" != "$original_pid" ]
}
wait_until "woodward kicks the frozen seedbank" 60 woodward_kicked_it

new_pid="$(ssh_out "pgrep -f '[s]ervice seedbank'")"
assert_success "seedbank has a new pid after healing" test -n "$new_pid"

heartbeat_after_heal="$(ssh_out sudo cat "$HEARTBEAT_PATH")"
heartbeat_has_advanced() {
    [ "$(ssh_out sudo cat "$HEARTBEAT_PATH")" != "$heartbeat_after_heal" ]
}
wait_until "seedbank heartbeat resumes ticking after healing" 20 heartbeat_has_advanced

assert_success "seedbank control socket is listening again" ssh_out \
    "sudo ss -xlp | grep -q '/run/douglas/seedbank/seedbank.sock'"

woodward_log_after="$(ssh_out sudo tail -n "+$((woodward_log_lines_before + 1))" "$WOODWARD_LOG")"
assert_contains "woodward's log recorded kicking seedbank" \
    "$woodward_log_after" "Kicking service seedbank"
assert_contains "woodward's log recorded a successful kick" \
    "$woodward_log_after" "Kicked service seedbank successfully"

finish
