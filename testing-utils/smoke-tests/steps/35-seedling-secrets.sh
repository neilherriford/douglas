#!/usr/bin/env bash
# Exercises OpenBao secrets access end to end via example-seedlings/secrets:
# push triggers bract to mint a scoped AppRole and stand up a second, sidecar
# container (doug-agent.secrets) running OpenBao Agent to hold it. The app
# container itself never receives role_id/secret_id — it only gets an
# OPENBAO_AGENT_ADDR env var pointing at that sidecar, and talks to it via a
# real Vault client library (node-vault) with no token of its own. A
# successful curl (with the value changing between two calls) is proof the
# whole chain worked live, not just that the unit tests mocked it correctly.
# Not itself a new CLI command, so no `# covers:` line (see 25-push-image.sh
# for the same setup-only pattern).
#
# secrets is dropped again at the end of this step so later steps
# (40/50/60/80) can keep assuming hello-world is the only seedling around.
set -uo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"
source ../lib.sh

SEED_DIR="/var/lib/douglas/seedbank/seeds/secrets"

assert_success "create secrets seedling from spec" bash -c \
    "ssh -o LogLevel=ERROR -i '$SSH_KEY' '$VM' '~/douglas seedling new --name secrets' < '$REPO_ROOT/example-seedlings/secrets/default.toml'"

definition_content="$(ssh_out sudo cat "$SEED_DIR/seedling.toml")"
assert_contains "definition records secrets access" "$definition_content" "secrets"

assert_success "build secrets image" ssh_out \
    "cd /mnt/share/douglas/example-seedlings/secrets && docker build . --tag secrets"

assert_success "tag secrets image for the local registry" ssh_out \
    "docker tag secrets localhost:7376/secrets"

assert_success "push secrets image" ssh_out \
    "docker push localhost:7376/secrets"

wait_until "secrets reconcile finished (traefik route file appeared)" 30 ssh_out \
    "sudo test -f /var/lib/douglas/mounts/traefik/config/dynamic/secrets.yml"

wait_until "secrets' agent container is running" 15 ssh_out \
    "docker inspect --format '{{.State.Running}}' doug-agent.secrets | grep -q true"

system_network_ip="$(ssh_out "docker inspect --format '{{(index .NetworkSettings.Networks \"douglas-system\").IPAddress}}' doug-agent.secrets")"
private_network_ip="$(ssh_out "docker inspect --format '{{(index .NetworkSettings.Networks \"doug.secrets\").IPAddress}}' doug-agent.secrets")"

assert_failure "agent's proxy is unreachable from the shared douglas-system network" ssh_out \
    "timeout 2 bash -c 'cat < /dev/null > /dev/tcp/$system_network_ip/8100'"
assert_success "agent's proxy is reachable from its own seedling's private network" ssh_out \
    "timeout 2 bash -c 'cat < /dev/null > /dev/tcp/$private_network_ip/8100'"

assert_owner_group_mode "secrets' openbao agent mount dir" \
    "/var/lib/douglas/mounts/secrets/openbao-agent" \
    "doug-secrets-openbao-ag-5579decb:doug-secrets-openbao-ag-5579decb:2770"

assert_success "secrets' openbao agent mount is a RAM-backed tmpfs" ssh_out \
    "findmnt --noheadings --output FSTYPE /var/lib/douglas/mounts/secrets/openbao-agent | grep -q '^tmpfs$'"

assert_success "agent's role_id credential file was written" ssh_out \
    "sudo test -s /var/lib/douglas/mounts/secrets/openbao-agent/role_id"
assert_success "agent's rendered config file was written" ssh_out \
    "sudo test -s /var/lib/douglas/mounts/secrets/openbao-agent/agent.json"

wait_until "agent auto-authed and removed its own secret_id file" 15 ssh_out \
    "sudo test ! -e /var/lib/douglas/mounts/secrets/openbao-agent/secret_id"

mounts_listing="$(ssh_out sudo ls /var/lib/douglas/mounts/secrets)"
if [ "$mounts_listing" = "openbao-agent" ]; then
    pass "secrets' own live mount tree holds nothing but the agent's subdirectory"
else
    fail "secrets' own live mount tree holds nothing but the agent's subdirectory (got: $mounts_listing)"
    FAILURES=$((FAILURES + 1))
fi

wait_until "secrets responds through traefik with HTTP 200" 15 ssh_out \
    "[ \"\$(curl -s -o /dev/null -w '%{http_code}' http://secrets.localhost/)\" = 200 ]"

first_response="$(ssh_out curl -s http://secrets.localhost/)"
assert_contains "first response reports a successful round trip" "$first_response" \
    "hello from openbao"

sleep 1
second_response="$(ssh_out curl -s http://secrets.localhost/)"
assert_contains "second response reports a successful round trip" "$second_response" \
    "hello from openbao"

if [ -n "$first_response" ] && [ "$first_response" != "$second_response" ]; then
    pass "consecutive responses carry different timestamps (proves a live write, not a cache)"
else
    fail "consecutive responses carry different timestamps (got '$first_response' twice)"
    FAILURES=$((FAILURES + 1))
fi

assert_success "stop secrets seedling before dropping" ssh_out \
    "~/douglas seedling stop --name secrets"

assert_failure "secrets' agent container is no longer running after stop" ssh_out \
    "docker inspect --format '{{.State.Running}}' doug-agent.secrets | grep -q true"

assert_success "drop secrets seedling" ssh_out "~/douglas seedling drop --name secrets"

assert_failure "secrets seedling dir no longer exists" ssh_out \
    "sudo test -e '$SEED_DIR'"

assert_failure "secrets' own live mount dir no longer exists" ssh_out \
    "sudo test -e /var/lib/douglas/mounts/secrets"

assert_failure "secrets' openbao agent ram disk no longer appears in the mount table" ssh_out \
    "findmnt /var/lib/douglas/mounts/secrets/openbao-agent"

assert_failure "secrets' agent container no longer exists after drop" ssh_out \
    "docker inspect doug-agent.secrets"

finish
