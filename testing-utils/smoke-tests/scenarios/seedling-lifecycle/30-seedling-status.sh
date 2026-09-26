#!/usr/bin/env bash
# covers: seedling status
set -uo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"
source ../../lib.sh

status="$(ssh_out '~/douglas' seedling status --name hello-world)"
assert_contains "status reports the seedling is running after push" "$status" "running"

assert_success "hello-world container is running" ssh_out \
    "docker ps --filter name=doug.hello-world --filter status=running -q | grep -q ."

# sudo: /var/lib/douglas/mounts/traefik/... is owned by the traefik service
# account, not traversable by the unprivileged dev user (same reasoning as
# assert_owner_group_mode in lib.sh).
assert_success "traefik route file was written" ssh_out \
    "sudo test -f /var/lib/douglas/mounts/traefik/config/dynamic/hello-world.yml"

# `douglas status`'s traefik section is a direct read of that same dynamic
# routes dir, so it should now list the route it just confirmed exists above.
overall_status="$(ssh_out '~/douglas --output-style plain status')"
assert_contains "douglas status reports the hello-world traefik route" \
    "$overall_status" "hello-world"

# hello-world reaches bract's docker only through resin's push-triggered
# reconcile (trigger_reconcile in bract/src/lib.rs), never through the
# explicit `reconcile_seedling` RPC — the two used to diverge, with only the
# RPC path setting desired_run_status to Running after bringing the
# container up. trigger_reconcile silently left it at its Stopped default,
# so hello-world looked fine immediately but got stopped by the very next
# watchdog sweep (every 30s) since desired=Stopped while running. Sleeping
# past one full sweep and re-checking is the only way to catch that: the
# assertions above run within a second of the push and would pass either
# way. Waits for a sweep to be logged as finished after this point rather
# than sleeping a fixed time.
BRACT_LOG="/var/log/douglas/bract/bract.log"
bract_log_lines_before="$(log_line_count "$BRACT_LOG")"

watchdog_sweep_finished() {
    local new_lines
    new_lines="$(ssh_out sudo tail -n "+$((bract_log_lines_before + 1))" "$BRACT_LOG")"
    [[ "$new_lines" == *"Running watchdog sweep outcome=ok"* ]]
}
wait_until "a watchdog sweep ran after the push-only reconcile" 45 watchdog_sweep_finished

assert_success "hello-world container survives a watchdog sweep after push-only reconcile" ssh_out \
    "docker ps --filter name=doug.hello-world --filter status=running -q | grep -q ."

finish
