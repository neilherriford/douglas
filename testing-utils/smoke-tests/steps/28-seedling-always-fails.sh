#!/usr/bin/env bash
# Exercises example-seedlings/always-fails — a seedling whose health check
# command (`false`) can never pass. Push/reconcile builds and starts its
# container the same as any other seedling (reconcile's own container-start
# path never consults health_check at all — see reconcile_seedling.rs's
# StartContainer), so it comes up and routes fine at first. The actual
# health check only runs through `seedling start`, so this step stops it
# and re-starts it six times in a row (start_seedling.rs's
# reached_max_fail_count threshold is fail_count > 5) to exercise the full
# give-up path: each of the first five failures leaves the container
# running (only the fail count climbs), and the sixth is the one that
# actually stops the container.
#
# Reuses hello-world's already-built image (no Dockerfile of its own), same
# as second-app in 27-seedling-subdomain.sh. Dropped again at the end so
# later steps' single-seedling assumptions still hold.
set -uo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"
source ../lib.sh

SEED_DIR="/var/lib/douglas/seedbank/seeds/always-fails"

assert_success "create always-fails seedling from spec" bash -c \
    "ssh -o LogLevel=ERROR -i '$SSH_KEY' '$VM' '~/douglas seedling new --name always-fails' < '$REPO_ROOT/example-seedlings/always-fails/default.toml'"

definition_content="$(ssh_out sudo cat "$SEED_DIR/seedling.toml")"
assert_contains "definition records the failing health check command" "$definition_content" \
    'command = "false"'
assert_contains "definition records the health check wait time" "$definition_content" \
    "wait_time_in_seconds = 1"

assert_success "tag hello-world image as always-fails" ssh_out \
    "docker tag hello-world localhost:7376/always-fails"

assert_success "push always-fails image" ssh_out \
    "docker push localhost:7376/always-fails"

wait_until "always-fails reconcile finished (traefik route file appeared)" 30 ssh_out \
    "sudo test -f /var/lib/douglas/mounts/traefik/config/dynamic/always-fails.yml"

wait_until "always-fails responds through traefik with HTTP 200" 15 ssh_out \
    "[ \"\$(curl -s -o /dev/null -w '%{http_code}' http://always-fails.localhost/)\" = 200 ]"

assert_success "stop always-fails seedling" ssh_out \
    "~/douglas seedling stop --name always-fails"

## Five failed attempts: the container gets (re)started each time and the
## health check fails, but the fail count hasn't crossed the threshold yet,
## so start_seedling.rs never stops the container back down.
for attempt in 1 2 3 4 5; do
    assert_failure "start attempt $attempt is refused" ssh_out \
        "~/douglas seedling start --name always-fails"
done

assert_success "always-fails container is still running after 5 failures" ssh_out \
    "docker ps --filter name=doug.always-fails --filter status=running -q | grep -q ."

## increment_health_log_fail_count only persists the count while it's below
## the give-up threshold, so after 5 recorded failures the file should read
## exactly 5 (see seedbank/src/lib.rs).
health_log_content="$(ssh_out sudo cat "$SEED_DIR/health.log")"
assert_contains "health check failure count reached 5" "$health_log_content" '"fail_count":5'

## The sixth failure crosses fail_count > 5 — this is the one that actually
## stops the container.
assert_failure "start attempt 6 is refused" ssh_out \
    "~/douglas seedling start --name always-fails"

assert_success "always-fails container was stopped after reaching max failures" ssh_out \
    "! docker ps --filter name=doug.always-fails --filter status=running -q | grep -q ."

## increment_health_log_fail_count skips writing once the threshold is
## crossed, so the persisted count stays frozen at 5 rather than advancing
## to 6 — this is the file's final state, not a missed update.
health_log_content="$(ssh_out sudo cat "$SEED_DIR/health.log")"
assert_contains "health check failure count stayed frozen at 5" "$health_log_content" '"fail_count":5'

assert_success "drop always-fails seedling" ssh_out "~/douglas seedling drop --name always-fails"

assert_failure "always-fails seedling dir no longer exists" ssh_out \
    "sudo test -e '$SEED_DIR'"

finish
