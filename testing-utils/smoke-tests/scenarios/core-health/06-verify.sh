#!/usr/bin/env bash
# covers: verify
#
# `douglas verify` checks a candidate binary's signature against the
# public key embedded in douglas itself, with no dependency on the running
# system — so this runs right after 05-build.sh deploys a freshly
# built-and-signed binary, before douglas is even started. Exercises the
# actual security property (tampered/unsigned binaries are rejected), not
# just the happy path.
set -uo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"
source ../../lib.sh

assert_success "verify accepts the freshly signed binary" ssh_out \
    "~/douglas verify --path ~/douglas"

verify_output="$(ssh_out "~/douglas --output-style plain verify --path ~/douglas")"
assert_contains "verify reports the signed version on stdout" "$verify_output" \
    "is signed correctly (v"
assert_contains "verify reports the data format and core versions" "$verify_output" \
    "data format 1, core openbao 1, traefik 1"

verify_json="$(ssh_out "~/douglas --output-style json verify --path ~/douglas")"
assert_contains "verify --output-style json reports success" "$verify_json" '"success":true'
assert_contains "verify --output-style json reports the data format" "$verify_json" '"format":1'
assert_contains "verify --output-style json reports the core versions" "$verify_json" \
    '"core":{"openbao":1,"traefik":1}'

assert_success "make a tampered copy" ssh_out \
    "cp ~/douglas ~/douglas-tampered && printf '\xff' | dd of=~/douglas-tampered bs=1 seek=1000 count=1 conv=notrunc"

assert_failure "verify rejects a tampered binary" ssh_out \
    "~/douglas verify --path ~/douglas-tampered"

tampered_output="$(ssh_out "~/douglas --output-style plain verify --path ~/douglas-tampered 2>&1")"
assert_contains "verify explains why the tampered binary was rejected" "$tampered_output" \
    "signature does not match"

assert_success "make an unsigned copy by stripping the signature trailer" ssh_out \
    "head -c -72 ~/douglas > ~/douglas-unsigned"

assert_failure "verify rejects a binary with no signature trailer" ssh_out \
    "~/douglas verify --path ~/douglas-unsigned"

assert_success "clean up test copies" ssh_out \
    "rm -f ~/douglas-tampered ~/douglas-unsigned"

finish
