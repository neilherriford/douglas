#!/usr/bin/env bash
# Builds, tags, and pushes the hello-world image on the VM, then waits for
# bract's push-triggered reconcile to bring the container up. Not a douglas
# CLI command itself, so it carries no `# covers:` line — the coverage test
# only requires every live CLI command to be claimed by *some* step, not
# every step to claim a command.
#
# example-seedlings/hello-world/ lives inside this same repo checkout, already visible to the
# VM at /mnt/share/douglas/example-seedlings/hello-world via the UTM share
# (same as 05-build.sh) — nothing to copy over first.
set -uo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"
source ../lib.sh

assert_success "build hello-world image" ssh_out \
    "cd /mnt/share/douglas/example-seedlings/hello-world && docker build . --tag hello-world"

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

# Reconcile also materializes the seedling's own declared mounts under
# /var/lib/douglas/mounts/<name>/ (distinct from seedbank's own copy under
# seedbank/seeds/<name>/mounts/, already checked by 20-seedling-new.sh),
# owned by hello-world's deterministic rolodex service account — see
# bract/src/rolodex/naming.rs's derive_system_name ("doug-" + normalized
# name + first 4 bytes of sha256(name) as hex, same derivation as
# "doug-traefik-858bd5f5" in 10-start.sh).
assert_owner_group_mode "hello-world's own public mount dir" \
    "/var/lib/douglas/mounts/hello-world/public" \
    "doug-hello-world-afa27b44:doug-hello-world-afa27b44:2770"

mount_content="$(ssh_out sudo cat /var/lib/douglas/mounts/hello-world/public/index.html)"
assert_contains "hello-world's live mount has the expected content" "$mount_content" \
    "Hello, world!"

# Traefik reloads its dynamic config from file on a watch, which can lag
# very slightly behind the route file landing — poll rather than a single
# shot. Route is Host(`localhost`) on entryPoint "web" (port 80, per
# traefik's static config in src/bootstrap/core_seedlings.rs).
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
