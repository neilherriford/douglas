#!/usr/bin/env bash
# covers: rollback
#
# A kept version that expects a different core seedling version than the one
# installed cannot be rolled back to: the older binary would run against
# containers the newer one changed. Runs after 86-rollback.sh, so the install
# is the version from 05-build.sh, and plants a kept version one step older
# than it whose OpenBao core version differs.
set -uo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"
source ../lib.sh

BIN_DIR="/var/lib/douglas/bin"

installed_version="$(ssh_out "grep -m1 '^version' /mnt/share/douglas/Cargo.toml | sed -E 's/version = \"(.*)\"/\1/'")"
IFS='.' read -r major minor patch <<<"$installed_version"

installed_trailer_version() {
    ssh_out "sudo tail -c 75 $BIN_DIR/douglas | head -c 3 | od -An -tu1 | xargs"
}

assert_equals "the installed version is the one from the build" "$major $minor $patch" "$(installed_trailer_version)"

assert_success "copy the built binary as the older one-way version" ssh_out \
    "cp $BUILT_BINARY ~/douglas-0.0.0"
assert_success "sign it as 0.0.0 with a different OpenBao core version" ssh_out \
    "$XTASK_SIGN ~/douglas-0.0.0 --version 0.0.0 --core openbao=2"
assert_success "keep it beside the installed binary" ssh_out \
    "sudo mv ~/douglas-0.0.0 $BIN_DIR/douglas-0.0.0"

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
