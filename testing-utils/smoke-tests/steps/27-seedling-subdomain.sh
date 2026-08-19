#!/usr/bin/env bash
# Exercises a second, subdomain-routed seedling alongside the root-routed
# hello-world from 20-seedling-new.sh/25-push-image.sh — the actual point of
# subdomain routing: two seedlings up at once without colliding on
# Host(`localhost`). Also checks the flip side: a *third* root-routed
# seedling must be rejected, since only one seedling can hold the root route
# at a time. Not itself a new CLI command (new/push are already covered), so
# no `# covers:` line, same reasoning as 25-push-image.sh.
#
# second-app is dropped again at the end of this step so later steps
# (30/40/50/60/80) can keep assuming hello-world is the only seedling around.
set -uo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"
source ../lib.sh

SEED_DIR="/var/lib/douglas/seedbank/seeds/second-app"

assert_success "create second-app seedling from subdomain spec" bash -c \
    "ssh -o LogLevel=ERROR -i '$SSH_KEY' '$VM' '~/douglas seedling new --name second-app' < '$REPO_ROOT/hello-world/subdomain.toml'"

definition_content="$(ssh_out sudo cat "$SEED_DIR/seedling.toml")"
assert_contains "definition records subdomain routing" "$definition_content" 'route = "subdomain"'

## Only one seedling can hold the root route at a time — a second `new` with
## a root-routed spec while hello-world already holds it must be rejected,
## not silently steal the claim.
THIRD_SEED_DIR="/var/lib/douglas/seedbank/seeds/third-app"

new_output="$(ssh -o LogLevel=ERROR -i "$SSH_KEY" "$VM" '~/douglas seedling new --name third-app' \
    < "$REPO_ROOT/hello-world/default.toml" 2>&1)"
new_status=$?
if [ "$new_status" -ne 0 ]; then
    pass "second root-routed seedling is rejected"
else
    fail "second root-routed seedling is rejected (expected failure, command succeeded)"
    FAILURES=$((FAILURES + 1))
fi
assert_contains "rejection names the current default holder" "$new_output" \
    "Default is already claimed by hello-world"

## `new` still writes the seedling record before the rejected ClaimDefault
## step runs, so third-app is now orphaned the same way it would be for any
## user hitting this collision by hand. `seedling drop` can't clean it up
## (its RemoveTraefikRoute step is unconditional and 500s when no route was
## ever written — a separate known bug), so remove it directly, same
## workaround used for this exact case during manual testing.
ssh_out sudo rm -rf "$THIRD_SEED_DIR" >/dev/null
assert_failure "third-app seedling dir no longer exists" ssh_out \
    "sudo test -e '$THIRD_SEED_DIR'"

# second-app reuses the hello-world image already built by 25-push-image.sh —
# the two seedlings only differ by the mounted index.html, not the image.
assert_success "tag hello-world image as second-app" ssh_out \
    "docker tag hello-world localhost:7376/second-app"

assert_success "push second-app image" ssh_out \
    "docker push localhost:7376/second-app"

wait_until "second-app reconcile finished (traefik route file appeared)" 30 ssh_out \
    "sudo test -f /var/lib/douglas/mounts/traefik/config/dynamic/second-app.yml"

route_content="$(ssh_out sudo cat /var/lib/douglas/mounts/traefik/config/dynamic/second-app.yml)"
assert_contains "second-app route uses its own subdomain host rule" "$route_content" \
    'Host(`second-app.localhost`)'

wait_until "second-app responds through traefik with HTTP 200" 15 ssh_out \
    "[ \"\$(curl -s -o /dev/null -w '%{http_code}' http://second-app.localhost/)\" = 200 ]"

## The actual collision check: both seedlings routable at once, each serving
## its own content — proves the Host rules don't shadow one another.
root_response="$(ssh_out curl -s http://localhost/)"
assert_contains "root route still serves hello-world's own content" "$root_response" \
    "from default"

subdomain_response="$(ssh_out curl -s http://second-app.localhost/)"
assert_contains "subdomain route serves second-app's own content" "$subdomain_response" \
    "from subdomain"

## Clean up so later steps' assumptions about hello-world being the only
## seedling still hold.
assert_success "stop second-app seedling before dropping" ssh_out \
    "~/douglas seedling stop --name second-app"

assert_success "drop second-app seedling" ssh_out "~/douglas seedling drop --name second-app"

assert_failure "second-app seedling dir no longer exists" ssh_out \
    "sudo test -e '$SEED_DIR'"

finish
