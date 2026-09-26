#!/usr/bin/env bash
# Exercises the rotate_logs opt-in flag through a user-declared mount —
# hello-world/default.toml's "log" mount — rather than a core service's
# hardcoded one. 16-log-rotation-happy.sh already proves the sweep itself
# works, against openbao's built-in log mount; this proves an ordinary
# seedling author can opt their own mount into rotation and have it work
# end to end.
set -uo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"
source ../../lib.sh

BRACT_LOG="/var/log/douglas/bract/bract.log"
LOG_FILE="/var/lib/douglas/mounts/hello-world/log/app.log"
ROTATED_LOG="${LOG_FILE}.1"
ROTATION_THRESHOLD_BYTES=$((10 * 1024 * 1024))

assert_success "no stale rotated hello-world log from a previous run" ssh_out \
    "sudo rm -f '$ROTATED_LOG'"

bract_log_lines_before="$(log_line_count "$BRACT_LOG")"

assert_success "pad hello-world's log past the rotation threshold" ssh_out \
    "sudo dd if=/dev/zero of='$LOG_FILE' bs=1M count=11 status=none"

log_rotated() {
    ssh_out "sudo test -f '$ROTATED_LOG'"
}
wait_until "bract rotates hello-world's oversized log" 90 log_rotated

rotated_size="$(ssh_out sudo stat -c%s "$ROTATED_LOG")"
assert_success "rotated log carries the oversized content" \
    test "$rotated_size" -ge "$ROTATION_THRESHOLD_BYTES"

bract_log_after="$(ssh_out sudo tail -n "+$((bract_log_lines_before + 1))" "$BRACT_LOG")"
assert_contains "bract's log recorded rotating hello-world's log" \
    "$bract_log_after" "Rotated 'hello-world' log file"

assert_no_log_errors "no warnings/errors while rotating hello-world's log" \
    "$BRACT_LOG" "$bract_log_lines_before"

finish
