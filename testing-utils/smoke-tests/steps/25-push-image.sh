#!/usr/bin/env bash
# Builds, tags, and pushes the hello-world image on the VM, then waits for
# bract's push-triggered reconcile to bring the container up. Not a douglas
# CLI command itself, so it carries no `# covers:` line — the coverage test
# only requires every live CLI command to be claimed by *some* step, not
# every step to claim a command.
#
# hello-world/ lives inside this same repo checkout, already visible to the
# VM at /mnt/share/douglas/hello-world via the UTM share (same as
# 05-build.sh) — nothing to copy over first.
set -uo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"
source ../lib.sh

assert_success "build hello-world image" ssh_out \
    "cd /mnt/share/douglas/hello-world && docker build . --tag hello-world"

assert_success "tag hello-world image for the local registry" ssh_out \
    "docker tag hello-world localhost:7376/hello-world"

assert_success "push hello-world image" ssh_out \
    "docker push localhost:7376/hello-world"

# Reconcile happens asynchronously off the push-trigger notification: bract
# builds/starts the container, then writes the traefik route file last in
# that chain — so polling for the route file is the single condition that
# implies the whole reconcile (container included) has finished.
wait_until "reconcile finished (traefik route file appeared)" 30 ssh_out \
    "sudo test -f /var/lib/douglas/mounts/traefik/config/dynamic/hello-world.yml"

# Reconcile creates the seedling's own docker network (doug.<name>) and
# joins traefik to it — see bract/src/blueprints/mod.rs's
# seedling_network_name and write_traefik_routes.rs.
assert_success "hello-world docker network exists" ssh_out \
    "docker network ls --filter name=doug.hello-world -q | grep -q ."

# Traefik reloads its dynamic config from file on a watch, which can lag
# very slightly behind the route file landing — poll rather than a single
# shot. Route is Host(`localhost`) on entryPoint "web" (port 80, per
# traefik's static config in src/bootstrap/seedlings.rs).
wait_until "hello-world responds through traefik with HTTP 200" 15 ssh_out \
    "[ \"\$(curl -s -o /dev/null -w '%{http_code}' http://localhost/)\" = 200 ]"

response="$(ssh_out curl -s http://localhost/)"
if [ -n "$response" ]; then
    pass "hello-world response through traefik has content"
else
    fail "hello-world response through traefik has content"
    FAILURES=$((FAILURES + 1))
fi

finish
