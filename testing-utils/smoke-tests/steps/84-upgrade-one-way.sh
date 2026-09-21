#!/usr/bin/env bash
# covers: upgrade
#
# A candidate that changes a core seedling's version cannot be rolled back
# from, so `upgrade` refuses it unless --allow-one-way is given. Only
# --plan-only is used here: the real upgrade is left to 85-upgrade.sh.
set -uo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"
source ../lib.sh

build_upgrade_candidate "~/douglas-one-way" --core openbao=2

refusal="$(ssh_out "sudo ~/douglas --output-style plain upgrade --plan-only --path ~/douglas-one-way" 2>&1)"
refused=$?
assert_not_equals "a one-way upgrade is refused without the opt-in" "0" "$refused"
assert_contains "the refusal names the changed core seedling" "$refusal" "core seedling 'openbao' version 1 -> 2"
assert_contains "the refusal names the opt-in" "$refusal" "--allow-one-way"

assert_success "a one-way upgrade plans with --allow-one-way" ssh_out \
    "sudo ~/douglas --output-style plain upgrade --plan-only --allow-one-way --path ~/douglas-one-way"

assert_success "the installed binary is untouched by planning" ssh_out \
    "sudo /var/lib/douglas/bin/douglas verify --path /var/lib/douglas/bin/douglas"

assert_success "clean up the candidate binary" ssh_out "rm -f ~/douglas-one-way"

finish
