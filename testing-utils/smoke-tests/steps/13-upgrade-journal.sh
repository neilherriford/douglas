#!/usr/bin/env bash
# covers: start
#
# `start` reports an upgrade whose process is gone (an interrupted upgrade),
# stays quiet while the recorded process is still running (the upgrade's own
# `start` runs then), and never refuses to start because of the journal.
# `status` shows the same journal, as interrupted or in progress, and nothing
# when there is none.
set -uo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"
source ../lib.sh

JOURNAL="/var/lib/douglas/upgrade-journal.json"
DEAD_PID=4194000

plant_journal() {
    local pid="$1"
    assert_success "plant an upgrade journal for process $pid" ssh_out \
        "echo '{\"from\":\"0.0.1\",\"to\":\"0.0.2\",\"previous_binary\":\"/var/lib/douglas/bin/douglas-0.0.1\",\"pid\":$pid}' | sudo tee '$JOURNAL' >/dev/null"
}

assert_failure "no upgrade journal exists to begin with" ssh_out "sudo test -e '$JOURNAL'"

quiet_status="$(ssh_out '~/douglas --output-style plain status')"
if [[ "$quiet_status" == *"Upgrade:"* ]]; then
    fail "status has no upgrade section when nothing is recorded"
    FAILURES=$((FAILURES + 1))
else
    pass "status has no upgrade section when nothing is recorded"
fi

plant_journal "$DEAD_PID"
if interrupted_output="$(ssh_out "sudo ~/douglas --output-style plain start" 2>&1)"; then
    pass "start still succeeds with an interrupted upgrade on record"
else
    fail "start still succeeds with an interrupted upgrade on record"
    echo "$interrupted_output" | sed 's/^/    /'
    FAILURES=$((FAILURES + 1))
fi
assert_contains "start reports the interrupted upgrade" "$interrupted_output" "did not finish"
assert_contains "the report names both versions" "$interrupted_output" "from 0.0.1 to 0.0.2"
assert_contains "the report says where the previous version is" "$interrupted_output" \
    "/var/lib/douglas/bin/douglas-0.0.1"
assert_success "start leaves the journal for whoever resolves it" ssh_out "sudo test -f '$JOURNAL'"

interrupted_status="$(ssh_out '~/douglas --output-style plain status')"
assert_contains "status has an upgrade section" "$interrupted_status" "Upgrade:"
assert_contains "status calls the upgrade interrupted" "$interrupted_status" "interrupted: from 0.0.1 to 0.0.2"
assert_contains "status says where the previous version is" "$interrupted_status" \
    "/var/lib/douglas/bin/douglas-0.0.1"

interrupted_json="$(ssh_out '~/douglas --output-style json status')"
assert_contains "status reports the interrupted upgrade as JSON" "$interrupted_json" '"state":"interrupted"'

live_pid="$(ssh_out "sleep 120 >/dev/null 2>&1 & echo \$!")"
assert_non_empty "a running process was started to stand in for the upgrade" "$live_pid"

plant_journal "$live_pid"
quiet_output="$(ssh_out "sudo ~/douglas --output-style plain start" 2>&1)"
if [[ "$quiet_output" == *"did not finish"* ]]; then
    fail "start is quiet while the recorded process is still running"
    FAILURES=$((FAILURES + 1))
else
    pass "start is quiet while the recorded process is still running"
fi

live_status="$(ssh_out '~/douglas --output-style plain status')"
assert_contains "status calls the upgrade in progress while its process is running" "$live_status" \
    "in progress: from 0.0.1 to 0.0.2"

assert_success "stop the stand-in process" ssh_out "kill $live_pid"
assert_success "remove the planted journal" ssh_out "sudo rm -f '$JOURNAL'"

finish
