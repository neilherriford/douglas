#!/usr/bin/env bash
set -uo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"
source ../lib.sh

MARKER="/var/lib/douglas/install-marker.json"

cargo_version="$(ssh_out "grep -m1 '^version' /mnt/share/douglas/Cargo.toml | sed -E 's/version = \"(.*)\"/\1/'")"

assert_success "the install marker exists after start" ssh_out "sudo test -f '$MARKER'"
assert_owner_group_mode "the install marker is root-owned" "$MARKER" "root:root:644"

marker="$(ssh_out "sudo cat '$MARKER'")"
assert_contains "the marker records the running version" "$marker" "\"version\":\"$cargo_version\""
assert_contains "the marker records the data format" "$marker" '"format":1'
assert_contains "the marker records the core seedling versions" "$marker" \
    '"core":{"openbao":1,"traefik":1}'

assert_success "keep a copy of the real marker" ssh_out "sudo cp '$MARKER' '$MARKER.saved'"

check_start_is_refused() {
    local description="$1" edit="$2" expected="$3"

    assert_success "plant a marker where $description" ssh_out \
        "sudo sed -i '$edit' '$MARKER'"

    if refusal="$(ssh_out "sudo ~/douglas --output-style plain start" 2>&1)"; then
        fail "start refuses when $description"
        FAILURES=$((FAILURES + 1))
    else
        pass "start refuses when $description"
    fi
    assert_contains "the refusal explains it ($description)" "$refusal" "$expected"

    assert_success "restore the real marker" ssh_out "sudo cp '$MARKER.saved' '$MARKER'"
}

check_start_is_refused "the data format is newer" 's/"format":1/"format":99/' "data format is 99"
check_start_is_refused "a core seedling is newer" 's/"openbao":1/"openbao":2/' "core seedling 'openbao'"

assert_success "start works again once the real marker is back" ssh_out \
    "sudo ~/douglas --output-style plain start"

marker_after="$(ssh_out "sudo cat '$MARKER'")"
assert_equals "the marker is unchanged by a successful start" "$marker" "$marker_after"

assert_success "clean up the saved marker" ssh_out "sudo rm -f '$MARKER.saved'"

finish
