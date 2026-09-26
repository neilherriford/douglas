#!/usr/bin/env bash
set -uo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"
source ../../lib.sh


section "scenario: woodward gives up on seedbank"

WOODWARD_LOG="/var/log/douglas/woodward/woodward.log"
FAILURE_MARKER="/run/douglas/woodward/seedbank.failed"
BINARY_PATH="/home/dev/douglas"
BINARY_BACKUP="/home/dev/douglas.smoke-backup"

original_pid="$(ssh_out "pgrep -f '[s]ervice seedbank'")"
assert_success "seedbank has a pid before the freeze" test -n "$original_pid"

woodward_log_lines_before="$(log_line_count "$WOODWARD_LOG")"

assert_success "break the binary link so kicks fail" ssh_out \
    "sudo mv '$BINARY_PATH' '$BINARY_BACKUP'"
assert_success "freeze seedbank so it needs kicking" ssh_out \
    "sudo kill -STOP $original_pid"

marker_written() {
    ssh_out "sudo test -f '$FAILURE_MARKER'" >/dev/null 2>&1
}
wait_until "woodward gives up on seedbank after repeated kick failures" 60 marker_written

marker_contents="$(ssh_out sudo cat "$FAILURE_MARKER")"
assert_contains "failure marker names seedbank" "$marker_contents" '"service_name":"seedbank"'
assert_contains "failure marker records 3 failed kicks" "$marker_contents" '"kick_failures":3'

woodward_log_after_giveup="$(ssh_out sudo tail -n "+$((woodward_log_lines_before + 1))" "$WOODWARD_LOG")"
assert_contains "woodward's log recorded 3 failed kicks" "$woodward_log_after_giveup" \
    "Failed to kick service seedbank"
assert_contains "woodward's log recorded giving up on seedbank" "$woodward_log_after_giveup" \
    "Gave up supervising seedbank after 0 restarts and 3 failed kicks"

woodward_log_lines_after_giveup="$(log_line_count "$WOODWARD_LOG")"
woodward_still_ticking() {
    local new_lines
    new_lines="$(ssh_out sudo tail -n "+$((woodward_log_lines_after_giveup + 1))" "$WOODWARD_LOG")"
    [[ "$new_lines" == *"Received timely heartbeat from bract"* ]] \
        && [[ "$new_lines" == *"Received timely heartbeat from resin"* ]]
}
wait_until "woodward keeps supervising bract and resin after giving up on seedbank" 15 \
    woodward_still_ticking

assert_success "unfreeze seedbank" ssh_out \
    "sudo kill -CONT $original_pid"

status_output="$(ssh_out "sudo '$BINARY_BACKUP' --output-style plain status")"
assert_contains "douglas status reports the supervisor gave up" "$status_output" \
    "supervisor gave up after 0 restarts and 3 failed kicks"

finish
