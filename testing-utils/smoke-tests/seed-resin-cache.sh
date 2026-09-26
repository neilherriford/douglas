#!/usr/bin/env bash
# Runs inside one disposable CI VM (see ci-fanout.sh): starts douglas for
# real so resin pulls the core images from Docker Hub once, also pulls the
# external image the smoke suite uses, then streams resin's repository
# directory out to $DOUGLAS_SMOKE_SEED_OUT on the host. Every later lane
# restores that tarball before starting douglas, so no lane talks to Docker
# Hub. Resin resolves tags from its own tag store and serves blobs
# cache-first, so a populated directory means zero upstream traffic.
set -uo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"
source lib.sh

: "${DOUGLAS_SMOKE_SEED_OUT:?set DOUGLAS_SMOKE_SEED_OUT to the host path to write the tarball to}"

EXTERNAL_IMAGE="$(grep -m1 '^reference' "$REPO_ROOT/example-seedlings/docker-hub-nginx/default.toml" | sed -E 's/reference *= *"docker.io\/(.*)"/\1/')"

bash setup/05-deploy.sh || exit 1

assert_success "douglas start pulls the core images through resin" ssh_out \
    "sudo ~/douglas --output-style plain start"

assert_success "pull the smoke suite's external image through resin" ssh_out \
    "docker pull localhost:7376/docker.io/$EXTERNAL_IMAGE"

assert_success "stop douglas so resin's cache is quiescent" ssh_out \
    "sudo ~/douglas --output-style plain stop"

[ "$FAILURES" -eq 0 ] || finish

ssh -o LogLevel=ERROR -i "$SSH_KEY" "$VM" \
    "sudo tar --numeric-owner -C /var/lib/douglas/resin -cf - repositories" >"$DOUGLAS_SMOKE_SEED_OUT.partial"
if [ "$?" -eq 0 ] && [ -s "$DOUGLAS_SMOKE_SEED_OUT.partial" ]; then
    mv "$DOUGLAS_SMOKE_SEED_OUT.partial" "$DOUGLAS_SMOKE_SEED_OUT"
    pass "resin cache saved to $DOUGLAS_SMOKE_SEED_OUT ($(du -h "$DOUGLAS_SMOKE_SEED_OUT" | cut -f1))"
else
    rm -f "$DOUGLAS_SMOKE_SEED_OUT.partial"
    fail "could not stream resin's repository directory out of the guest"
    FAILURES=$((FAILURES + 1))
fi

finish
