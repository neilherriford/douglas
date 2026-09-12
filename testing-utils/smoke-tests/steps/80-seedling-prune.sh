#!/usr/bin/env bash
# covers: seedling prune
set -uo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"
source ../lib.sh

SEED_DIR="/var/lib/douglas/seedbank/seeds/orphan-app"
MOUNT_DIR="/var/lib/douglas/mounts/orphan-app"
ROUTE_FILE="/var/lib/douglas/mounts/traefik/config/dynamic/orphan-app.yml"

assert_success "create orphan-app seedling from spec" bash -c \
    "ssh -o LogLevel=ERROR -i '$SSH_KEY' '$VM' '~/douglas seedling new --name orphan-app' < '$REPO_ROOT/example-seedlings/hello-world/default.toml'"

assert_success "tag hello-world image as orphan-app" ssh_out \
    "docker tag hello-world localhost:7376/orphan-app"

assert_success "push orphan-app image" ssh_out \
    "docker push localhost:7376/orphan-app"

wait_until "orphan-app reconcile finished (traefik route file appeared)" 30 ssh_out \
    "sudo test -f '$ROUTE_FILE'"

assert_success "orphan-app container is running before the orphan is created" ssh_out \
    "docker ps --filter name=doug.orphan-app --filter status=running -q | grep -q ."
assert_success "orphan-app docker network exists before the orphan is created" ssh_out \
    "docker network ls --filter name=doug.orphan-app -q | grep -q ."
assert_success "orphan-app mount dir exists before the orphan is created" ssh_out \
    "sudo test -d '$MOUNT_DIR'"

assert_success "delete orphan-app's seedbank record directly (simulating a lost record)" ssh_out \
    "sudo rm -rf '$SEED_DIR'"

prune_output="$(ssh_out '~/douglas' seedling prune --yes)"
assert_contains "prune output lists the deadwood container" "$prune_output" "orphan-app"
assert_contains "prune output confirms pruning happened" "$prune_output" "Pruned."

assert_failure "orphan-app container is gone after pruning" ssh_out \
    "docker ps -a --filter name=doug.orphan-app -q | grep -q ."
assert_failure "orphan-app docker network is gone after pruning" ssh_out \
    "docker network ls --filter name=doug.orphan-app -q | grep -q ."
assert_failure "orphan-app route file is gone after pruning" ssh_out \
    "sudo test -e '$ROUTE_FILE'"
assert_failure "orphan-app mount dir is gone after pruning" ssh_out \
    "sudo test -e '$MOUNT_DIR'"

rerun_output="$(ssh_out '~/douglas' seedling prune --yes)"
assert_contains "a second prune finds nothing left to do" "$rerun_output" "No deadwood found."

finish
