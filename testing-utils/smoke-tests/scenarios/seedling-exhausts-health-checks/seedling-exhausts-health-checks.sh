#!/usr/bin/env bash
set -uo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"
source ../../lib.sh


section "scenario: seedling exhausts its health checks"

SEED_DIR="/var/lib/douglas/seedbank/seeds/always-fails"

assert_success "build hello-world image" ssh_out \
    "cd $EXAMPLE_SEEDLINGS/hello-world && docker build . --tag hello-world"

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

BRACT_LOG="/var/log/douglas/bract/bract.log"
SWEEP_DONE="Running watchdog sweep outcome=ok"
sweeps_before="$(ssh_out "sudo grep -c '$SWEEP_DONE' $BRACT_LOG || true")"
wait_until "a watchdog sweep just finished, so the next is ~30s away" 45 ssh_out \
    "[ \"\$(sudo grep -c '$SWEEP_DONE' $BRACT_LOG)\" -gt ${sweeps_before:-0} ]"

assert_success "stop always-fails seedling" ssh_out \
    "~/douglas seedling stop --name always-fails"

fail_count() {
    ssh_out "sudo cat $SEED_DIR/health.log 2>/dev/null | grep -o '\"fail_count\":[0-9]*' | cut -d: -f2 || true"
}

attempt=0
while [ "$(fail_count)" != "4" ] && [ "$attempt" -lt 6 ]; do
    attempt=$((attempt + 1))
    assert_failure "start attempt $attempt is refused" ssh_out \
        "~/douglas seedling start --name always-fails"
done

assert_equals "health check failure count is 4" "4" "$(fail_count)"

assert_success "always-fails container is still running at 4 failures" ssh_out \
    "docker ps --filter name=doug.always-fails --filter status=running -q | grep -q ."

assert_failure "the start attempt that reaches the max is refused" ssh_out \
    "~/douglas seedling start --name always-fails"

health_log_content="$(ssh_out sudo cat "$SEED_DIR/health.log")"
assert_contains "health check failure count reached 5" "$health_log_content" '"fail_count":5'

assert_failure "start attempt 6 is refused" ssh_out \
    "~/douglas seedling start --name always-fails"

assert_success "always-fails container was stopped after reaching max failures" ssh_out \
    "! docker ps --filter name=doug.always-fails --filter status=running -q | grep -q ."

health_log_content="$(ssh_out sudo cat "$SEED_DIR/health.log")"
assert_contains "health check failure count stayed frozen at 5" "$health_log_content" '"fail_count":5'

finish
