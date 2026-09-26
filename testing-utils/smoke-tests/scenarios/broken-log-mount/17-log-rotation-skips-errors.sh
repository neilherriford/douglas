#!/usr/bin/env bash
set -uo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"
source ../../lib.sh

BRACT_LOG="/var/log/douglas/bract/bract.log"
LOG_MOUNT_DIR="/var/lib/douglas/mounts/openbao/log"
LOG_MOUNT_BACKUP="/var/lib/douglas/mounts/openbao/log.smoke-backup"

restore_log_mount() {
    ssh_out \
        "sudo rm -f '$LOG_MOUNT_DIR' && sudo mv '$LOG_MOUNT_BACKUP' '$LOG_MOUNT_DIR'"
}

assert_success "back up openbao's real log mount" ssh_out \
    "sudo mv '$LOG_MOUNT_DIR' '$LOG_MOUNT_BACKUP'"
assert_success "swap in a plain file where the log mount directory should be" ssh_out \
    "sudo touch '$LOG_MOUNT_DIR'"

bract_log_lines_before="$(log_line_count "$BRACT_LOG")"

warning_logged() {
    local new_lines
    new_lines="$(ssh_out sudo tail -n "+$((bract_log_lines_before + 1))" "$BRACT_LOG")"
    [[ "$new_lines" == *"Could not read log mount"* ]] && [[ "$new_lines" == *"openbao"* ]]
}
wait_until "bract's log rotation sweep warns about openbao's broken log mount" 90 warning_logged

assert_success "bract is still running after the broken mount" ssh_out \
    "pgrep -f '[s]ervice bract'"
assert_success "douglas status still responds after the broken mount" ssh_out \
    "sudo /home/dev/douglas --output-style plain status"

assert_success "restore openbao's real log mount" restore_log_mount

bract_log_lines_after_restore="$(log_line_count "$BRACT_LOG")"

sleep 65

bract_log_after_recovery="$(ssh_out sudo tail -n "+$((bract_log_lines_after_restore + 1))" "$BRACT_LOG")"
sweep_still_warning() {
    [[ "$bract_log_after_recovery" == *"Could not read log mount"* ]]
}
assert_failure "sweep no longer warns about openbao's log mount after restoring it" sweep_still_warning

finish
