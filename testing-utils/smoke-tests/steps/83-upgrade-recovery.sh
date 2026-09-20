#!/usr/bin/env bash
# covers: upgrade
#
# An upgrade to a version that will not start is undone: the previous
# binary is put back, the previous version is started again, and the
# command fails saying why. Runs before 85-upgrade.sh, while the install is
# still the version built by 05-build.sh, so that a higher-version
# candidate exists to fail with.
set -uo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"
source ../lib.sh

build_failing_upgrade_candidate "~/douglas-broken"

installed_trailer_version() {
    ssh_out "sudo tail -c 75 /var/lib/douglas/bin/douglas | head -c 3 | od -An -tu1 | xargs"
}

version_before="$(installed_trailer_version)"
traefik_started_at_before="$(ssh_out "docker inspect -f '{{.State.StartedAt}}' doug.traefik")"
woodward_pid_before="$(ssh_out "pgrep -f '[s]ervice woodward'")"
marker_before="$(ssh_out "sudo cat /var/lib/douglas/install-marker.json")"

assert_non_empty "the installed version baseline was captured" "$version_before"
assert_non_empty "traefik container baseline was captured" "$traefik_started_at_before"
assert_non_empty "woodward pid baseline was captured" "$woodward_pid_before"
assert_non_empty "install marker baseline was captured" "$marker_before"

upgrade_output="$(ssh_out "sudo ~/douglas --output-style plain upgrade --path ~/douglas-broken" 2>&1)"
upgrade_status=$?

assert_not_equals "upgrade to a version that will not start fails" "0" "$upgrade_status"
assert_contains "the failure says the new version failed to start" "$upgrade_output" "failed to start"
assert_contains "the failure includes what the new version printed" "$upgrade_output" "stub refusing to start"

assert_equals "the previous binary was put back" "$version_before" "$(installed_trailer_version)"

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

assert_success "clean up the stub candidate" ssh_out "rm -f ~/douglas-broken"

finish
