#!/usr/bin/env bash
# covers: upgrade
#
# A candidate that cannot be upgraded to is refused with the reason on the
# console, not just "Upgrade failed", and nothing is touched.
set -uo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"
source ../../lib.sh

installed_trailer_version() {
    ssh_out "sudo tail -c 75 /var/lib/douglas/bin/douglas | head -c 3 | od -An -tu1 | xargs"
}

version_before="$(installed_trailer_version)"
assert_non_empty "the installed version baseline was captured" "$version_before"

assert_success "write a candidate that was never signed" ssh_out \
    "printf '#!/bin/sh\nexit 0\n' > ~/douglas-unsigned && chmod +x ~/douglas-unsigned"

unsigned_output="$(ssh_out "sudo ~/douglas --output-style plain upgrade --path ~/douglas-unsigned" 2>&1)"
assert_not_equals "an unsigned candidate is refused" "0" "$?"
assert_contains "the refusal says it is not a valid douglas executable" "$unsigned_output" \
    "Not a valid douglas executable"
assert_contains "the refusal says why" "$unsigned_output" "is not a signed douglas binary"

missing_output="$(ssh_out "sudo ~/douglas --output-style plain upgrade --path ~/douglas-nowhere" 2>&1)"
assert_not_equals "a candidate that does not exist is refused" "0" "$?"
assert_contains "the refusal says there is no executable there" "$missing_output" \
    "No executable at given path"

assert_equals "the installed version is untouched" "$version_before" "$(installed_trailer_version)"
assert_failure "no upgrade journal was written" ssh_out "sudo test -e /var/lib/douglas/upgrade-journal.json"

assert_success "clean up the unsigned candidate" ssh_out "rm -f ~/douglas-unsigned"

finish
