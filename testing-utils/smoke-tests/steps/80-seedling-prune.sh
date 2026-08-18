#!/usr/bin/env bash
# covers: seedling prune
set -uo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"
source ../lib.sh

assert_success "prune orphans without prompting" ssh_out "~/douglas seedling prune --yes"

finish
