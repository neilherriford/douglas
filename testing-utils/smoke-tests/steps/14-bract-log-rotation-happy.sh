#!/usr/bin/env bash
set -uo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"
source ../lib.sh

BRACT_LOG="/var/log/douglas/bract/bract.log"
ROTATED_LOG="${BRACT_LOG}.1"
BINARY_PATH="/home/dev/douglas"
ROTATION_THRESHOLD_BYTES=$((10 * 1024 * 1024))

assert_success "no stale rotated bract log from a previous run" ssh_out \
    "sudo rm -f '$ROTATED_LOG'"

original_pid="$(ssh_out "pgrep -f '[s]ervice bract'")"
assert_success "bract has a pid before the rotation test" test -n "$original_pid"

assert_success "pad bract's own log past the rotation threshold" ssh_out \
    "sudo dd if=/dev/zero of='$BRACT_LOG' bs=1M count=11 oflag=append conv=notrunc status=none"

assert_success "kick bract so it reopens its own log file" ssh_out \
    "sudo '$BINARY_PATH' --output-style plain kick bract"

new_pid="$(ssh_out "pgrep -f '[s]ervice bract'")"
assert_success "bract has a new pid after the kick" test -n "$new_pid"
if [ "$new_pid" = "$original_pid" ]; then
    fail "bract has a new pid after the kick (got the same pid: $new_pid)"
    FAILURES=$((FAILURES + 1))
else
    pass "bract has a new pid after the kick"
fi

rotated_exists() {
    ssh_out "sudo test -f '$ROTATED_LOG'"
}
wait_until "bract rotates its own oversized log on restart" 15 rotated_exists

rotated_size="$(ssh_out sudo stat -c%s "$ROTATED_LOG")"
assert_success "rotated bract log carries the oversized content" \
    test "$rotated_size" -ge "$ROTATION_THRESHOLD_BYTES"

active_size="$(ssh_out sudo stat -c%s "$BRACT_LOG")"
assert_success "bract's active log file is fresh after rotating" \
    test "$active_size" -lt "$ROTATION_THRESHOLD_BYTES"

finish
