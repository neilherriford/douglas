#!/usr/bin/env bash
# covers: seedling drop
#
# Verifies drop actually undoes everything 20-seedling-new.sh and
# 25-push-image.sh checked into existence: every seedbank file/dir, the
# docker network, the traefik route file, and routability through traefik.
set -uo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"
source ../lib.sh

SEED_DIR="/var/lib/douglas/seedbank/seeds/hello-world"

# drop requires the container to already be stopped — 50-seedling-start.sh
# left it running, so stop it first. Not claimed via `# covers:` here since
# 40-seedling-stop.sh already covers that command; this is just setup.
assert_success "stop hello-world seedling before dropping" ssh_out \
    "~/douglas seedling stop --name hello-world"

assert_success "drop hello-world seedling" ssh_out "~/douglas seedling drop --name hello-world"

## Reverses every file/dir 20-seedling-new.sh checked into existence —
## seedbank's `delete` removes the whole seedling dir in one shot (see
## seedbank/src/lib.rs), so these should all be gone now.
GONE_CHECKS=(
    "$SEED_DIR|seedling dir"
    "$SEED_DIR/id|seedling id file"
    "$SEED_DIR/version|seedling version file"
    "$SEED_DIR/seedling.toml|seedling definition file"
    "$SEED_DIR/mounts|mounts dir"
    "$SEED_DIR/mounts/public|public mount dir"
    "$SEED_DIR/mounts/public/index.html|public mount's index.html"
)

for entry in "${GONE_CHECKS[@]}"; do
    IFS='|' read -r path desc <<<"$entry"
    assert_failure "$desc no longer exists" ssh_out "sudo test -e '$path'"
done

# release_default deletes the pointer file outright (not just clears its
# contents) when it currently names this seedling — see
# seedbank/src/lib.rs's release_default.
assert_failure "default-seedling pointer file no longer exists" ssh_out \
    "sudo test -e /var/lib/douglas/seedbank/seeds/default"

## Reverses 25-push-image.sh's checks: network gone, route file gone,
## container gone, no longer routable.
assert_failure "hello-world docker network no longer exists" ssh_out \
    "docker network ls --filter name=doug.hello-world -q | grep -q ."

assert_failure "hello-world container no longer exists" ssh_out \
    "docker inspect doug.hello-world"

assert_failure "traefik route file no longer exists" ssh_out \
    "sudo test -f /var/lib/douglas/mounts/traefik/config/dynamic/hello-world.yml"

# Drop also wipes hello-world's own live mount dir (distinct from
# seedbank's copy, already checked above) — see
# bract/src/blueprints/drop_seedling.rs's DeleteSeedlingMounts.
assert_failure "hello-world's own live mount dir no longer exists" ssh_out \
    "sudo test -e /var/lib/douglas/mounts/hello-world"

wait_until "hello-world no longer responds through traefik with HTTP 200" 15 ssh_out \
    "[ \"\$(curl -s -o /dev/null -w '%{http_code}' http://localhost/)\" != 200 ]"

finish
