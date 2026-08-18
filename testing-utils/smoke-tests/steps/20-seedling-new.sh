#!/usr/bin/env bash
# covers: seedling new
#
# `seedling new` only writes seedbank's own on-disk record (see
# seedbank/src/lib.rs's `create`/`write_definition`/`write_mounts`/
# `claim_default`) — no docker container/network and no rolodex service
# account get created yet, since those only happen at reconcile time
# (triggered later by a push, exercised by 25-push-image.sh). This step
# checks both halves of that: everything seedbank should have written down,
# and that nothing beyond seedbank happened prematurely.
set -uo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"
source ../lib.sh

SEED_DIR="/var/lib/douglas/seedbank/seeds/hello-world"

# No `--file`: the CLI reads stdin when the flag is omitted (see its help
# text), not when passed a literal "-".
assert_success "create hello-world seedling from spec" bash -c \
    "ssh -o LogLevel=ERROR -i '$SSH_KEY' '$VM' '~/douglas seedling new --name hello-world' < '$REPO_ROOT/hello-world/seedling.toml'"

## Everything seedbank should have written to disk for this seedling.
# "path|owner:group:mode|description" — seedbank runs as douglas-seedbank
# and owns every file/dir it creates.
FILE_CHECKS=(
    "$SEED_DIR|douglas-seedbank:douglas-seedbank:750|seedling dir"
    "$SEED_DIR/id|douglas-seedbank:douglas-seedbank:640|seedling id file"
    "$SEED_DIR/version|douglas-seedbank:douglas-seedbank:640|seedling version file"
    "$SEED_DIR/seedling.toml|douglas-seedbank:douglas-seedbank:640|seedling definition file"
    "$SEED_DIR/mounts|douglas-seedbank:douglas-seedbank:750|mounts dir"
    "$SEED_DIR/mounts/public|douglas-seedbank:douglas-seedbank:750|public mount dir"
    "$SEED_DIR/mounts/public/index.html|douglas-seedbank:douglas-seedbank:640|public mount's index.html"
    "/var/lib/douglas/seedbank/seeds/default|douglas-seedbank:douglas-seedbank:640|default-seedling pointer file"
)

for entry in "${FILE_CHECKS[@]}"; do
    IFS='|' read -r path expected desc <<<"$entry"
    assert_owner_group_mode "$desc" "$path" "$expected"
done

## Content checks — confirm the definition round-tripped correctly, not
## just that some file exists.
version_content="$(ssh_out sudo cat "$SEED_DIR/version")"
assert_contains "seedling version is 1" "$version_content" "1"

default_content="$(ssh_out sudo cat "/var/lib/douglas/seedbank/seeds/default")"
assert_contains "default-seedling pointer names hello-world" "$default_content" "hello-world"

definition_content="$(ssh_out sudo cat "$SEED_DIR/seedling.toml")"
assert_contains "definition records Routed routing" "$definition_content" "routing.Routed"
assert_contains "definition records the declared public port" "$definition_content" "public = 3000"

mount_content="$(ssh_out sudo cat "$SEED_DIR/mounts/public/index.html")"
assert_contains "mounted index.html has the expected content" "$mount_content" "Hello, world!"

## Negative checks: reconcile hasn't run yet, so none of its side effects
## should exist — catches a regression where `new` accidentally starts
## provisioning infrastructure instead of just recording the definition.
assert_failure "no rolodex user exists for hello-world yet" ssh_out \
    "getent passwd | grep -q hello-world"
assert_failure "no rolodex group exists for hello-world yet" ssh_out \
    "getent group | grep -q hello-world"
assert_failure "no docker network exists for hello-world yet" ssh_out \
    "docker network ls | grep -q hello-world"
assert_failure "no docker container exists for hello-world yet" ssh_out \
    "docker ps -a | grep -q hello-world"

finish
