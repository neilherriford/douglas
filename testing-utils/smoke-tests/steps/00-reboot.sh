#!/usr/bin/env bash
# Reboots the VM back to the live-CD's pristine boot state before anything
# else runs, then waits for SSH to come back up. Not a douglas CLI command
# itself, so no `# covers:` line (see 25-push-image.sh for the same
# setup-only pattern).
#
# Set DOUGLAS_SMOKE_SKIP_REBOOT=1 to skip this (e.g. while iterating quickly
# against a VM you just rebooted by hand).
set -uo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"
source ../lib.sh

if [ -n "${DOUGLAS_SMOKE_SKIP_REBOOT:-}" ]; then
    echo "DOUGLAS_SMOKE_SKIP_REBOOT set, skipping reboot"
    finish
fi

REBOOT_TIMEOUT="${DOUGLAS_SMOKE_REBOOT_TIMEOUT:-120}"

echo "Rebooting $VM to a clean state..."
# The reboot always looks like a failed command from ssh's perspective —
# the connection drops mid-session rather than exiting cleanly — so this
# isn't asserted.
ssh_out "sudo reboot" >/dev/null 2>&1 || true

# Give the VM a moment to actually go down before polling, so a connection
# attempt that lands before the machine has started shutting down doesn't
# report "up" against the boot we're trying to leave behind.
sleep 5

deadline=$((SECONDS + REBOOT_TIMEOUT))
until ssh -o ConnectTimeout=5 -o BatchMode=yes -i "$SSH_KEY" "$VM" true >/dev/null 2>&1; do
    if [ "$SECONDS" -ge "$deadline" ]; then
        fail "VM did not come back up within ${REBOOT_TIMEOUT}s"
        FAILURES=$((FAILURES + 1))
        finish
    fi
    sleep 3
done

pass "VM rebooted and SSH is back up"

finish
