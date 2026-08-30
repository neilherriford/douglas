#!/usr/bin/env bash
# covers: seedling status
set -uo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"
source ../lib.sh

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

finish
