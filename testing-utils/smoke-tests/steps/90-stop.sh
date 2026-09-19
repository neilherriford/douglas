#!/usr/bin/env bash
# covers: stop
set -uo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"
source ../lib.sh

if stop_output="$(ssh_out "sudo ~/douglas --output-style plain stop" 2>&1)"; then
    pass "douglas stop"
else
    fail "douglas stop"
    echo "$stop_output" | sed 's/^/    /'
    FAILURES=$((FAILURES + 1))
fi
assert_contains "douglas stop reports the result on stdout" "$stop_output" "Douglas stopped."

for service in woodward bract seedbank resin; do
    assert_failure "$service is no longer running" \
        ssh_out "pgrep -f '[s]ervice $service'"
done

assert_success "no douglas-managed containers are still running" ssh_out \
    "! docker ps --filter name=doug --filter status=running -q | grep -q ."

finish
