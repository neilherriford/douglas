#!/usr/bin/env bash
set -uo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"
source ../../lib.sh


section "scenario: upgrade from a different filesystem than the install target"

CANDIDATE="/dev/shm/WhatInTheWorldWhoNamedThis.exe"
BINARY_LINK="/var/lib/douglas/bin/douglas"

install_target="$(ssh_out "readlink -f '$BINARY_LINK'")"
assert_non_empty "the binary link resolves to an install target" "$install_target"

candidate_device="$(ssh_out "stat -c %d /dev/shm")"
target_device="$(ssh_out "stat -c %d \"\$(dirname '$install_target')\"")"
assert_not_equals "the candidate's filesystem differs from the install target's" \
    "$candidate_device" "$target_device"

build_upgrade_candidate "$CANDIDATE"

if upgrade_output="$(ssh_out "sudo ~/douglas --output-style plain upgrade --path '$CANDIDATE'" 2>&1)"; then
    pass "douglas upgrade completes"
else
    fail "douglas upgrade completes"
    echo "$upgrade_output" | sed 's/^/    /'
    FAILURES=$((FAILURES + 1))
fi

[ "$FAILURES" -eq 0 ] || finish

assert_contains "the new binary's start ran with the requested output style" \
    "$upgrade_output" "Douglas started."

installed_version="$(ssh_out "sudo tail -c 75 '$install_target' | head -c 3 | od -An -tu1 | xargs")"
assert_equals "the install target carries the new version" \
    "$major $minor $((patch + 1))" "$installed_version"

assert_success "the install target verifies" ssh_out \
    "sudo '$install_target' verify --path '$install_target'"

owner_group="$(ssh_out "sudo stat -L -c '%U:%G' '$BINARY_LINK'")"
assert_equals "the installed binary is owned by root:douglas-admin" "root:douglas-admin" "$owner_group"

assert_success "the installed binary is executable" ssh_out "sudo test -x '$BINARY_LINK'"
assert_success "the binary link is still a symlink" ssh_out "test -L '$BINARY_LINK'"

assert_failure "the candidate was removed after being copied into place" ssh_out \
    "test -e '$CANDIDATE'"
assert_failure "no staging file was left next to the install target" ssh_out \
    "test -e '$install_target.upgrade'"

finish
