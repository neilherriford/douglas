#!/usr/bin/env bash
# covers: seedling start
set -uo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"
source ../lib.sh

assert_success "start hello-world seedling" ssh_out "~/douglas seedling start --name hello-world"

assert_success "hello-world container is running again" ssh_out \
    "docker ps --filter name=doug.hello-world --filter status=running -q | grep -q ."

# Confirms traefik can reach it again, not just that the container object
# reports running — mirrors 25-push-image.sh's checks.
wait_until "hello-world responds through traefik with HTTP 200 again" 15 ssh_out \
    "[ \"\$(curl -s -o /dev/null -w '%{http_code}' http://localhost/)\" = 200 ]"

response="$(ssh_out curl -s http://localhost/)"
if [ -n "$response" ]; then
    pass "hello-world response through traefik has content again"
else
    fail "hello-world response through traefik has content again"
    FAILURES=$((FAILURES + 1))
fi

finish
