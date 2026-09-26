#!/usr/bin/env bash
# covers: upgrade
#
# A new version whose `start` claims success but leaves nothing running is
# caught by the health confirmation, and the upgrade is undone the same way
# as when start itself fails. The stub's `start` exits 0 without starting
# anything, and the upgrade has already killed the previous services, so
# none of them is alive when their health is checked.
set -uo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"
source ../../lib.sh

build_stub_upgrade_candidate "~/douglas-hollow" "exit 0"

installed_trailer_version() {
    ssh_out "sudo tail -c 75 /var/lib/douglas/bin/douglas | head -c 3 | od -An -tu1 | xargs"
}

version_before="$(installed_trailer_version)"
traefik_started_at_before="$(ssh_out "docker inspect -f '{{.State.StartedAt}}' doug.traefik")"
marker_before="$(ssh_out "sudo cat /var/lib/douglas/install-marker.json")"

assert_non_empty "the installed version baseline was captured" "$version_before"
assert_non_empty "traefik container baseline was captured" "$traefik_started_at_before"
assert_non_empty "install marker baseline was captured" "$marker_before"

upgrade_output="$(ssh_out "sudo ~/douglas --output-style plain upgrade --path ~/douglas-hollow" 2>&1)"
upgrade_status=$?

assert_not_equals "upgrade to a version that leaves nothing running fails" "0" "$upgrade_status"
assert_contains "the failure says the new version is not healthy" "$upgrade_output" "is not healthy after the upgrade"

assert_equals "the previous binary was put back" "$version_before" "$(installed_trailer_version)"

assert_failure "no upgrade journal is left once the upgrade is settled" ssh_out \
    "sudo test -e /var/lib/douglas/upgrade-journal.json"

assert_success "the restored binary verifies" ssh_out \
    "sudo /var/lib/douglas/bin/douglas verify --path /var/lib/douglas/bin/douglas"

wait_until "woodward is running again under the previous version" 30 \
    ssh_out "pgrep -f '[s]ervice woodward'"
wait_until "resin is running again under the previous version" 30 \
    ssh_out "pgrep -f '[s]ervice resin'"
wait_until "seedbank is running again under the previous version" 30 \
    ssh_out "pgrep -f '[s]ervice seedbank'"

assert_equals "traefik container was never restarted" \
    "$traefik_started_at_before" "$(ssh_out "docker inspect -f '{{.State.StartedAt}}' doug.traefik")"

assert_equals "the install marker still records the previous version" \
    "$marker_before" "$(ssh_out "sudo cat /var/lib/douglas/install-marker.json")"

assert_success "clean up the stub candidate" ssh_out "rm -f ~/douglas-hollow"

finish
