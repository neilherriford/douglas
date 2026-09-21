#!/usr/bin/env bash
# covers: rollback
#
# A kept version that expects a different core seedling version than the one
# installed cannot be rolled back to: the older binary would run against
# containers the newer one changed. Runs after 86-rollback.sh, so the install
# is the version from 05-build.sh, and plants a kept version one step older
# than it whose OpenBao core version differs.
#
# Like the version bump in build_upgrade_candidate, the edits to the shared
# checkout only last long enough to build and sign, and are reverted
# unconditionally.
set -uo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"
source ../lib.sh

BIN_DIR="/var/lib/douglas/bin"
CONFIG="/mnt/share/douglas/config/src/lib.rs"
CARGO_TOML="/mnt/share/douglas/Cargo.toml"

installed_version="$(ssh_out "grep -m1 '^version' $CARGO_TOML | sed -E 's/version = \"(.*)\"/\1/'")"
IFS='.' read -r major minor patch <<<"$installed_version"

revert_checkout() {
    ssh_out "sed -i 's/OPENBAO_VERSION: u16 = 2;/OPENBAO_VERSION: u16 = 1;/' $CONFIG" >/dev/null 2>&1
    ssh_out "sed -i 's/^version = \"0.0.0\"/version = \"$installed_version\"/' $CARGO_TOML" >/dev/null 2>&1
}
trap revert_checkout EXIT

installed_trailer_version() {
    ssh_out "sudo tail -c 75 $BIN_DIR/douglas | head -c 3 | od -An -tu1 | xargs"
}

assert_equals "the installed version is the one from the build" "$major $minor $patch" "$(installed_trailer_version)"

assert_success "bump the OpenBao core version" ssh_out \
    "sed -i 's/OPENBAO_VERSION: u16 = 1;/OPENBAO_VERSION: u16 = 2;/' $CONFIG"
assert_success "lower the version to 0.0.0" ssh_out \
    "sed -i 's/^version = \"$installed_version\"/version = \"0.0.0\"/' $CARGO_TOML"
assert_success "build and sign the older one-way version" ssh_out \
    "cd /mnt/share/douglas && cargo run -p xtask --quiet -- build"
assert_success "keep it beside the installed binary" ssh_out \
    "sudo cp /mnt/share/cache/target/debug/douglas $BIN_DIR/douglas-0.0.0"

revert_checkout
assert_success "the OpenBao core version was reverted" ssh_out "grep -q 'OPENBAO_VERSION: u16 = 1;' $CONFIG"
assert_success "the version was reverted" ssh_out "grep -q '^version = \"$installed_version\"' $CARGO_TOML"

woodward_pid_before="$(ssh_out "pgrep -f '[s]ervice woodward'")"
assert_non_empty "woodward pid baseline was captured" "$woodward_pid_before"

refusal="$(ssh_out "sudo ~/douglas --output-style plain rollback" 2>&1)"
assert_not_equals "rolling back across a core seedling change is refused" "0" "$?"
assert_contains "the refusal says the version cannot be rolled back to" "$refusal" \
    "Cannot roll back to that version"
assert_contains "the refusal names the core seedling that differs" "$refusal" \
    "core seedling 'openbao' version 1 -> 2"

explicit="$(ssh_out "sudo ~/douglas --output-style plain rollback --to 0.0.0" 2>&1)"
assert_not_equals "asking for that version explicitly is refused too" "0" "$?"
assert_contains "the explicit refusal names the core seedling" "$explicit" "core seedling 'openbao' version 1 -> 2"

assert_equals "the installed version is untouched" "$major $minor $patch" "$(installed_trailer_version)"
assert_equals "woodward was never restarted" "$woodward_pid_before" "$(ssh_out "pgrep -f '[s]ervice woodward'")"
assert_failure "no staging file is left behind" ssh_out "sudo test -e $BIN_DIR/douglas-rollback.staging"
assert_failure "no upgrade journal was written" ssh_out "sudo test -e /var/lib/douglas/upgrade-journal.json"

assert_success "remove the planted version" ssh_out "sudo rm -f $BIN_DIR/douglas-0.0.0"

finish
