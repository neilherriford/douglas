#!/usr/bin/env bash
set -uo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"
source ../../lib.sh

BRACT_LOG="/var/log/douglas/bract/bract.log"
AUDIT_LOG="/var/lib/douglas/mounts/openbao/log/openbao_audit.log"
ROTATED_LOG="${AUDIT_LOG}.1"
ROTATION_THRESHOLD_BYTES=$((10 * 1024 * 1024))

assert_success "no stale rotated audit log from a previous run" ssh_out \
    "sudo rm -f '$ROTATED_LOG'"

bract_log_lines_before="$(log_line_count "$BRACT_LOG")"

assert_success "pad openbao's audit log past the rotation threshold" ssh_out \
    "sudo dd if=/dev/zero of='$AUDIT_LOG' bs=1M count=11 status=none"

audit_log_rotated() {
    ssh_out "sudo test -f '$ROTATED_LOG'"
}
wait_until "bract rotates openbao's oversized audit log" 90 audit_log_rotated

rotated_size="$(ssh_out sudo stat -c%s "$ROTATED_LOG")"
assert_success "rotated audit log carries the oversized content" \
    test "$rotated_size" -ge "$ROTATION_THRESHOLD_BYTES"

bract_log_after="$(ssh_out sudo tail -n "+$((bract_log_lines_before + 1))" "$BRACT_LOG")"
assert_contains "bract's log recorded rotating openbao's audit log" \
    "$bract_log_after" "Rotated 'openbao' log file"

assert_no_log_errors "no warnings/errors while rotating openbao's audit log" \
    "$BRACT_LOG" "$bract_log_lines_before"

finish
