#!/usr/bin/env bash
# covers: seedling stop
set -uo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"
source ../lib.sh

assert_success "stop hello-world seedling" ssh_out "~/douglas seedling stop --name hello-world"

assert_success "hello-world container is no longer running" ssh_out \
    "! docker ps --filter name=doug.hello-world --filter status=running -q | grep -q ."

# The route file/network aren't touched by stop (only drop removes those),
# but traefik can no longer reach the stopped backend — confirms the stop
# actually took effect end-to-end, not just that the container object
# reports stopped.
wait_until "hello-world no longer responds through traefik with HTTP 200" 15 ssh_out \
    "[ \"\$(curl -s -o /dev/null -w '%{http_code}' http://localhost/)\" != 200 ]"

finish
