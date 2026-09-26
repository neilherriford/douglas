#!/usr/bin/env bash
# covers: rollback
#
# Runs right after 85-upgrade.sh, which leaves the new version installed and
# the version it replaced kept beside it, so there is something to roll back
# to. Like the upgrade, rolling back must never touch running containers.
set -uo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"
source ../../lib.sh

BIN_DIR="/var/lib/douglas/bin"
MARKER="/var/lib/douglas/install-marker.json"

installed_trailer_version() {
    ssh_out "sudo tail -c 75 $BIN_DIR/douglas | head -c 3 | od -An -tu1 | xargs"
}

old_version="$(cargo_version)"
IFS='.' read -r major minor patch <<<"$old_version"
new_version="$major.$minor.$((patch + 1))"

assert_equals "the upgraded version is installed" "$major $minor $((patch + 1))" "$(installed_trailer_version)"
assert_success "the version the upgrade replaced is kept" ssh_out "sudo test -x $BIN_DIR/douglas-$old_version"

plan_output="$(ssh_out "sudo ~/douglas --output-style plain rollback --plan-only" 2>&1)"
plan_status=$?
assert_equals "planning a rollback succeeds" "0" "$plan_status"
assert_equals "planning leaves the upgraded version installed" \
    "$major $minor $((patch + 1))" "$(installed_trailer_version)"
assert_failure "planning leaves no staging file behind" ssh_out \
    "sudo test -e $BIN_DIR/douglas-rollback.staging"

not_kept="$(ssh_out "sudo ~/douglas --output-style plain rollback --to 0.0.0" 2>&1)"
assert_not_equals "rolling back to a version that is not kept is refused" "0" "$?"
assert_contains "the refusal says it is not kept" "$not_kept" "is not kept"
assert_contains "the refusal lists what is kept" "$not_kept" "$old_version"

not_older="$(ssh_out "sudo ~/douglas --output-style plain rollback --to $new_version" 2>&1)"
assert_not_equals "rolling back to the running version is refused" "0" "$?"
assert_contains "the refusal says it is not older" "$not_older" "not older"

not_a_version="$(ssh_out "sudo ~/douglas --output-style plain rollback --to latest" 2>&1)"
assert_contains "a value that is not a version is refused with an explanation" "$not_a_version" "is not a version"

traefik_started_at_before="$(ssh_out "docker inspect -f '{{.State.StartedAt}}' doug.traefik")"
woodward_pid_before="$(ssh_out "pgrep -f '[s]ervice woodward'")"
assert_non_empty "traefik container baseline was captured" "$traefik_started_at_before"
assert_non_empty "woodward pid baseline was captured" "$woodward_pid_before"

if rollback_output="$(ssh_out "sudo ~/douglas --output-style plain rollback" 2>&1)"; then
    pass "douglas rollback completes"
else
    fail "douglas rollback completes"
    echo "$rollback_output" | sed 's/^/    /'
    FAILURES=$((FAILURES + 1))
fi

assert_contains "the rolled-back version's start ran with the requested output style" \
    "$rollback_output" "Douglas started."

[ "$FAILURES" -eq 0 ] || finish

assert_equals "the previous version is installed again" "$major $minor $patch" "$(installed_trailer_version)"
assert_success "the installed binary verifies" ssh_out "sudo $BIN_DIR/douglas verify --path $BIN_DIR/douglas"

marker_after="$(ssh_out "sudo cat $MARKER")"
assert_contains "the install marker records the rolled-back version" "$marker_after" "\"version\":\"$old_version\""

assert_success "the version rolled back from is kept, so the rollback can be undone" ssh_out \
    "sudo test -x $BIN_DIR/douglas-$new_version"
assert_failure "no staging file is left behind" ssh_out "sudo test -e $BIN_DIR/douglas-rollback.staging"
assert_failure "no upgrade journal is left once the rollback is settled" ssh_out \
    "sudo test -e /var/lib/douglas/upgrade-journal.json"

woodward_pid_after="$(ssh_out "pgrep -f '[s]ervice woodward'")"
assert_not_equals "woodward restarted under a new pid" "$woodward_pid_before" "$woodward_pid_after"

assert_equals "traefik container was never restarted during the rollback" \
    "$traefik_started_at_before" "$(ssh_out "docker inspect -f '{{.State.StartedAt}}' doug.traefik")"

finish
