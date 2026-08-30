#!/usr/bin/env bash
# covers: status
#
# `douglas status` asks bract (which runs privileged and holds douglas's
# own OpenBao AppRole credentials) to decrypt them, log in, and inspect
# live state — so it must work for the unprivileged `dev` user, no sudo,
# same as every other seedling command. Runs right after 10-start.sh,
# while traefik/openbao are the only seedlings and OpenBao is freshly
# bootstrapped, and checks the real decrypted values (mounts, approle,
# acme, root ca), not just that the command exited 0.
set -uo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"
source ../lib.sh

status="$(ssh_out '~/douglas --output-style plain status')"

assert_contains "reports traefik as a seedling" "$status" "traefik: running"
assert_contains "reports openbao as a seedling" "$status" "openbao: running"

assert_contains "bract is reported running" "$status" "bract: running"
assert_contains "resin is reported running" "$status" "resin: running"
assert_contains "seedbank is reported running" "$status" "seedbank: running"

# no seedling has been pushed through traefik yet at this point in the run
assert_contains "traefik has no routes yet" "$status" "routes: none"

assert_contains "openbao is running" "$status" "running: true"
assert_contains "openbao is initialized" "$status" "initialized: true"
assert_contains "openbao is unsealed" "$status" "sealed: false"
assert_contains "douglas's own credentials decrypt and work" "$status" "credentials work: true"
assert_contains "approle auth is enabled" "$status" "approle enabled: true"
assert_contains "acme is enabled" "$status" "acme enabled: true"
assert_contains "root ca is configured" "$status" "root ca configured: true"
assert_contains "acme pki role was created" "$status" "acme pki role created: true"
assert_contains "kv mount is listed" "$status" "kv/ (kv)"
assert_contains "pki mount is listed" "$status" "pki/ (pki)"

json="$(ssh_out '~/douglas --output-style json status')"
assert_contains "json output parses as an object with an openbao key" "$json" '"openbao":'

finish
