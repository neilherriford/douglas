#!/usr/bin/env bash
# Captures a host-side serial console device (UTM's Pseudo-TTY serial
# device, exposed as /dev/ttysNNN on macOS) to a timestamped log file, so a
# VM wedge during an unattended run can be diagnosed after the fact instead
# of needing someone watching live when it happens.
#
# A guest-level reboot (steps/00-reboot.sh's `sudo reboot`) does NOT tear
# down the host-side pty this reads from — the serial device belongs to the
# VM process itself, not the guest OS, so this keeps capturing straight
# through every reboot in a run, including whatever happens during a wedge.
#
# A FULL VM restart (build-image.sh's `utmctl stop --force` + `start`) does
# tear the pty down and UTM hands out a new path on the next start, which
# this script has no way to discover on its own. If you restart the VM
# fully while this is running, stop it (Ctrl+C) and start a new one against
# the new path once you've found it.
#
# Prints every captured line to stdout as it arrives (in addition to the
# log file), so running this in the foreground doubles as a live `tail -f`
# of the console — no need for a second terminal watching the log file too.
#
# Usage:
#   ./capture-serial.sh /dev/ttys022
#   ./capture-serial.sh /dev/ttys022 ~/my-log.log
#
# To run unattended overnight (no live view, just the log file):
#   nohup ./capture-serial.sh /dev/ttys022 > /dev/null 2>&1 &
#   disown
set -uo pipefail

PORT="${1:?usage: $0 <serial-device-path> [log-file]}"
LOG_FILE="${2:-$HOME/douglas-serial.log}"

log() {
    echo "$(date '+%Y-%m-%d %H:%M:%S') $1" | tee -a "$LOG_FILE"
}

log "[capture] starting, watching $PORT, logging to $LOG_FILE"

while true; do
    if [ ! -e "$PORT" ]; then
        log "[capture] $PORT does not exist — waiting..."
        sleep 5
        continue
    fi

    log "[capture] attached to $PORT"

    while IFS= read -r line; do
        echo "$(date '+%Y-%m-%d %H:%M:%S') $line"
    done < "$PORT" | tee -a "$LOG_FILE"

    log "[capture] $PORT closed/disconnected — will retry"
    sleep 2
done
