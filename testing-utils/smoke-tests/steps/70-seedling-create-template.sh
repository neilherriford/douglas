#!/usr/bin/env bash
# covers: seedling create-template
set -uo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"
source ../lib.sh

template="$(ssh_out '~/douglas' seedling create-template)"
assert_contains "template includes a mounts section" "$template" "[mounts"
assert_contains "template includes a ports section" "$template" "[ports]"

finish
