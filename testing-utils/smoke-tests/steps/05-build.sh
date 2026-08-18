#!/usr/bin/env bash
# Builds douglas from the shared source checkout and deploys the binary to
# ~/douglas on the VM, mirroring the manual build-and-test workflow. Not a
# douglas CLI command itself, so no `# covers:` line (see 25-push-image.sh
# for the same setup-only pattern).
#
# /mnt/share/douglas is the same checkout as this repo, made visible to the
# VM via UTM's shared-folder mount, so there's nothing to copy over first —
# same reason 25-push-image.sh can build hello-world/ directly from there
# too, instead of copying it over.
set -uo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"
source ../lib.sh

# Stop any previously-running douglas before rebuilding/redeploying over it.
# Not asserted: "no matching process" (the normal not-yet-running case) is
# expected, not a failure.
ssh_out "sudo pkill -x douglas" >/dev/null 2>&1 || true

assert_success "build douglas" ssh_out \
    "cd /mnt/share/douglas && cargo build"

assert_success "deploy douglas binary to home directory" ssh_out \
    "cp /mnt/share/douglas/target/debug/douglas ~/douglas"

finish
