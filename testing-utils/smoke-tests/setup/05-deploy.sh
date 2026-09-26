#!/usr/bin/env bash
# Deploys the douglas binary to ~/douglas on the VM. Nothing is built or
# signed here: ci-fanout.sh cross-compiles and signs once on the host and
# shares that one file read-only across every scenario. Not a douglas CLI
# command itself, so no `# covers:` line.
set -uo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"
source ../lib.sh

: "${DOUGLAS_SMOKE_PREBUILT_BINARY:?set DOUGLAS_SMOKE_PREBUILT_BINARY to the signed douglas binary to deploy (ci-fanout.sh does this)}"

ssh_out "sudo pkill -x douglas" >/dev/null 2>&1 || true

assert_success "deploy the prebuilt, pre-signed douglas binary" \
    deploy_prebuilt_binary "$DOUGLAS_SMOKE_PREBUILT_BINARY"

finish
