#!/usr/bin/env bash
# Covers the other half of ImageSource: a seedling whose image is an
# external reference (docker.io/nginxinc/nginx-unprivileged:1.27), not
# something pushed through resin. The unprivileged image is required because
# douglas runs seedling containers as a non-root service account, which
# stock nginx doesn't tolerate. Not itself a new CLI command (new/start are
# already covered), so
# no `# covers:` line, same reasoning as 25-push-image.sh.
#
# Two things this proves that no other step does:
#   1. Reconcile actually pulls the image through resin's own pull-through
#      cache (ProxyingBlobStore), not straight from Docker Hub — confirmed
#      by checking resin's on-disk upstream/docker.io tree gets populated.
#   2. Pushing is still rejected for an upstream-shaped repository path,
#      even now that reading one is supported — the read/write asymmetry
#      the whole external-image feature depends on (see
#      resin/src/authorize.rs's authorize_write / require_local).
#
# docker-hub-nginx is dropped again at the end so later steps (30/40/50/60/
# 80) can keep assuming hello-world is the only seedling around.
set -uo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"
source ../../lib.sh

SEED_DIR="/var/lib/douglas/seedbank/seeds/docker-hub-nginx"

assert_success "create docker-hub-nginx seedling from external-image spec" bash -c \
    "ssh -o LogLevel=ERROR -i '$SSH_KEY' '$VM' '~/douglas seedling new --name docker-hub-nginx' < '$REPO_ROOT/example-seedlings/docker-hub-nginx/default.toml'"

definition_content="$(ssh_out sudo cat "$SEED_DIR/seedling.toml")"
assert_contains "definition records an external image source" "$definition_content" \
    'type = "external"'
assert_contains "definition records the docker.io reference" "$definition_content" \
    "docker.io/nginxinc/nginx-unprivileged:1.27"

## `new` only ever writes seedbank's own record — there's no push to
## trigger reconcile for an external image, so nothing should have started
## yet (mirrors 20-seedling-new.sh's own negative checks for the pushed
## case).
assert_failure "no docker container exists for docker-hub-nginx yet" ssh_out \
    "docker ps -a | grep -q docker-hub-nginx"

## An explicit `start` is what triggers reconcile here, since there's no
## push event to hang it off.
assert_success "start docker-hub-nginx seedling" ssh_out \
    "~/douglas seedling start --name docker-hub-nginx"

wait_until "reconcile finished (traefik route file appeared)" 60 ssh_out \
    "sudo test -f /var/lib/douglas/mounts/traefik/config/dynamic/docker-hub-nginx.yml"

## The container's image is resin's own registry + the upstream-folded
## path, never docker.io directly — proving the pull went through resin,
## not around it.
container_image="$(ssh_out docker inspect --format '{{.Config.Image}}' doug.docker-hub-nginx)"
assert_contains "container image is routed through resin's own registry" "$container_image" \
    "localhost:7376/docker.io/nginxinc/nginx-unprivileged"

## The actual cache-population check: resin's on-disk upstream tree for
## docker.io now exists. Checking the top-level per-upstream-host directory
## rather than a specific blob path keeps this independent of nginx's own
## image digest, which isn't pinned by this test and can change upstream.
wait_until "resin's upstream/docker.io cache directory was populated" 15 ssh_out \
    "sudo test -d /var/lib/douglas/resin/repositories/upstream/docker.io"

wait_until "docker-hub-nginx responds through traefik with HTTP 200" 15 ssh_out \
    "[ \"\$(curl -s -o /dev/null -w '%{http_code}' http://docker-hub-nginx.localhost/)\" = 200 ]"

response="$(ssh_out curl -s http://docker-hub-nginx.localhost/)"
assert_contains "response is nginx's own default page" "$response" "Welcome to nginx"

## Push-rejection: an upstream-shaped repository path must still refuse a
## write, even though it's now readable through the cache. POST is the
## earliest point authorize_write runs for any write flow (see
## resin/src/upload.rs's start()), so a single request is enough — no need
## to actually complete an upload to prove the rejection.
push_status="$(ssh_out curl -s -o /dev/null -w '%{http_code}' -X POST \
    http://localhost:7376/v2/docker.io/nginxinc/nginx-unprivileged/blobs/uploads/)"
if [ "$push_status" = "405" ]; then
    pass "push to an upstream-shaped repository is rejected with 405"
else
    fail "push to an upstream-shaped repository is rejected with 405 (got $push_status)"
    FAILURES=$((FAILURES + 1))
fi

## Clean up so later steps' assumptions about hello-world being the only
## (pushed) seedling still hold.
assert_success "stop docker-hub-nginx seedling before dropping" ssh_out \
    "~/douglas seedling stop --name docker-hub-nginx"

assert_success "drop docker-hub-nginx seedling" ssh_out \
    "~/douglas seedling drop --name docker-hub-nginx"

assert_failure "docker-hub-nginx seedling dir no longer exists" ssh_out \
    "sudo test -e '$SEED_DIR'"

finish
