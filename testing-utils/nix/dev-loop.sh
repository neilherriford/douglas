#!/usr/bin/env bash
# SSHes into the dev host interactively; each time that session exits,
# reboots it (plain `sudo shutdown -r now` over a second ssh call - nothing
# hypervisor-specific, works against any ssh-reachable box) and reconnects
# once it's back up. Note this only restarts the OS: anything written to
# disk during the session survives the reboot, this isn't a snapshot
# revert. Ctrl+C during the "waiting" phases exits the whole loop.
set -uo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"

SSH_KEY="${DOUGLAS_SMOKE_SSH_KEY:-$(pwd)/ssh-keys/douglas_id_ed25519}"
SSH_TARGET="${DOUGLAS_SMOKE_VM:-dev@douglas-dev.local}"

ssh_probe() {
    ssh -o LogLevel=ERROR -o ConnectTimeout=2 -o BatchMode=yes \
        -i "$SSH_KEY" "$SSH_TARGET" true 2>/dev/null
}

wait_for_down() {
    echo "Waiting for $SSH_TARGET to go down..."
    until ! ssh_probe; do
        sleep 1
    done
}

wait_for_up() {
    echo "Waiting for $SSH_TARGET to come back up..."
    until ssh_probe; do
        sleep 2
    done
}

while true; do
    ssh -i "$SSH_KEY" "$SSH_TARGET"

    echo "SSH session ended. Rebooting $SSH_TARGET..."
    ssh -o LogLevel=ERROR -i "$SSH_KEY" "$SSH_TARGET" "sudo shutdown -r now" || true

    wait_for_down
    wait_for_up
done
