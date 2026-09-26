#!/usr/bin/env bash
set -uo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"
source ../../lib.sh


section "scenario: watchdog stops a degrading seedling on its own"

SEED_DIR="/var/lib/douglas/seedbank/seeds/always-fails"
BRACT_LOG="/var/log/douglas/bract/bract.log"

assert_success "build hello-world image" ssh_out \
    "cd $EXAMPLE_SEEDLINGS/hello-world && docker build . --tag hello-world"

assert_success "create always-fails seedling from spec" bash -c \
    "ssh -o LogLevel=ERROR -i '$SSH_KEY' '$VM' '~/douglas seedling new --name always-fails' < '$REPO_ROOT/example-seedlings/always-fails/default.toml'"

assert_success "tag hello-world image as always-fails" ssh_out \
    "docker tag hello-world localhost:7376/always-fails"

bract_log_lines_before="$(log_line_count "$BRACT_LOG")"

assert_success "push always-fails image" ssh_out \
    "docker push localhost:7376/always-fails"

wait_until "always-fails reconcile finished (traefik route file appeared)" 30 ssh_out \
    "sudo test -f /var/lib/douglas/mounts/traefik/config/dynamic/always-fails.yml"

assert_success "always-fails container is running after push, before any health check" ssh_out \
    "docker ps --filter name=doug.always-fails --filter status=running -q | grep -q ."

container_stopped() {
    ! ssh_out "docker ps --filter name=doug.always-fails --filter status=running -q | grep -q ."
}
wait_until "watchdog stops always-fails on its own after repeated health check failures" \
    240 container_stopped

health_log_content="$(ssh_out sudo cat "$SEED_DIR/health.log")"
assert_contains "health check failure count reached 5" "$health_log_content" '"fail_count":5'

bract_log_after="$(ssh_out sudo tail -n "+$((bract_log_lines_before + 1))" "$BRACT_LOG")"
assert_contains "bract's log recorded always-fails' repeated health check failures" \
    "$bract_log_after" "Seedling 'always-fails' failed its health check"
assert_contains "bract's log recorded watchdog stopping always-fails on its own" \
    "$bract_log_after" \
    "Seedling 'always-fails' exceeded its maximum health check failures, stopping container"

status_output="$(ssh_out "~/douglas seedling status --name always-fails")"
assert_contains "seedling status no longer reports always-fails as running" \
    "$status_output" "defined"

finish
