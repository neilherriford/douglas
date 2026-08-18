#!/usr/bin/env bash
# covers: stop
set -uo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"
source ../lib.sh

# `douglas stop` is currently unimplemented (`todo!()` in src/main.rs), so
# this only exists to keep the coverage test honest about the command's
# existence — flip this to assert_success once it's actually implemented.
assert_failure "douglas stop is not yet implemented (update this step when it is)" \
    ssh_out "~/douglas stop"

finish
