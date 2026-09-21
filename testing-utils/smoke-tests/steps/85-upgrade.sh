#!/usr/bin/env bash
# covers: upgrade
#
# Runs before 90-stop.sh (not after) so the seedlings/containers from
# 20-seedling-new.sh / 50-seedling-start.sh are still up — the whole point
# of this step is to prove `douglas upgrade` never touches them
# (StopBract::new(false)), which a post-stop upgrade couldn't exercise.
#
# xtask signs whatever version is in the shared checkout's Cargo.toml, so
# to get a genuinely *higher*-version candidate binary (required for
# `create_plan`'s InvalidUpgrade check to pass) this bumps the patch
# version just long enough to build+sign, then reverts it — the shared
# checkout is live source other steps read, so this restores it
# unconditionally even if the build fails partway through.
set -uo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"
source ../lib.sh

build_upgrade_candidate "~/douglas-new"

## Baseline: capture what should survive the upgrade untouched, and what
## should be replaced. hello-world is already dropped by 60-seedling-drop.sh,
## so traefik (a core container started by `start`) is the survivor.

traefik_started_at_before="$(ssh_out "docker inspect -f '{{.State.StartedAt}}' doug.traefik")"
woodward_pid_before="$(ssh_out "pgrep -f '[s]ervice woodward'")"
resin_pid_before="$(ssh_out "pgrep -f '[s]ervice resin'")"
seedbank_pid_before="$(ssh_out "pgrep -f '[s]ervice seedbank'")"

assert_non_empty "traefik container baseline was captured" "$traefik_started_at_before"
assert_non_empty "woodward pid baseline was captured" "$woodward_pid_before"
assert_non_empty "resin pid baseline was captured" "$resin_pid_before"
assert_non_empty "seedbank pid baseline was captured" "$seedbank_pid_before"

## An ssh session that drops mid-upgrade sends the upgrade a hangup. Send it
## two while the upgrade runs; it must carry on and finish anyway.

assert_success "arrange for the upgrade to be hung up on partway through" ssh_out \
    "rm -f /tmp/hup-sent; (sleep 6; sudo pkill -HUP -f \"^\$HOME/douglas .*upgrade\" && echo sent >> /tmp/hup-sent; sleep 10; sudo pkill -HUP -f \"^\$HOME/douglas .*upgrade\" && echo sent >> /tmp/hup-sent) >/dev/null 2>&1 &"

if upgrade_output="$(ssh_out "sudo ~/douglas --output-style plain upgrade --path ~/douglas-new" 2>&1)"; then
    pass "douglas upgrade completes"
else
    fail "douglas upgrade completes"
    echo "$upgrade_output" | sed 's/^/    /'
    FAILURES=$((FAILURES + 1))
fi

# upgrade replaces itself with `start`, so the success message on stdout is
# start's, which only appears if it inherited --output-style plain.
assert_contains "the new binary's start ran with the requested output style" \
    "$upgrade_output" "Douglas started."

assert_non_empty "the upgrade was hung up on while it ran" "$(ssh_out "cat /tmp/hup-sent 2>/dev/null")"

# Everything below only means something if the upgrade actually ran.
[ "$FAILURES" -eq 0 ] || finish

## The installed binary is now the new, higher version, signed and owned
## correctly — read from the file's own trailer bytes (last 75 bytes are
## version(3) + signature(64) + magic(8)), since `verify` reports to its log
## file rather than stdout.

installed_version="$(ssh_out "sudo tail -c 75 /var/lib/douglas/bin/douglas | head -c 3 | od -An -tu1 | xargs")"
assert_equals "installed binary carries the new version" "$major $minor $((patch + 1))" "$installed_version"

marker_after="$(ssh_out "sudo cat /var/lib/douglas/install-marker.json")"
assert_contains "the install marker records the upgraded version" "$marker_after" \
    "\"version\":\"$NEW_VERSION\""

retained="/var/lib/douglas/bin/douglas-$CURRENT_VERSION"
retained_version="$(ssh_out "sudo tail -c 75 $retained | head -c 3 | od -An -tu1 | xargs")"
assert_equals "the previous version was kept for rollback" "$major $minor $patch" "$retained_version"

assert_success "the retained version verifies" ssh_out \
    "sudo /var/lib/douglas/bin/douglas verify --path $retained"

assert_success "the retained version is executable" ssh_out "sudo test -x $retained"

assert_failure "no partial copy is left behind" ssh_out \
    "sudo ls /var/lib/douglas/bin/*.partial"

assert_failure "no upgrade journal is left once the upgrade is settled" ssh_out \
    "sudo test -e /var/lib/douglas/upgrade-journal.json"

assert_success "installed binary verifies" ssh_out \
    "sudo /var/lib/douglas/bin/douglas verify --path /var/lib/douglas/bin/douglas"

owner_group="$(ssh_out "sudo stat -L -c '%U:%G' /var/lib/douglas/bin/douglas")"
assert_equals "installed binary is owned by root:douglas-admin" "root:douglas-admin" "$owner_group"

assert_success "installed binary is executable" ssh_out \
    "sudo test -x /var/lib/douglas/bin/douglas"

## Services were actually restarted under the new binary (different PIDs).

woodward_pid_after="$(ssh_out "pgrep -f '[s]ervice woodward'")"
resin_pid_after="$(ssh_out "pgrep -f '[s]ervice resin'")"
seedbank_pid_after="$(ssh_out "pgrep -f '[s]ervice seedbank'")"

assert_not_equals "woodward restarted under a new pid" "$woodward_pid_before" "$woodward_pid_after"
assert_not_equals "resin restarted under a new pid" "$resin_pid_before" "$resin_pid_after"
assert_not_equals "seedbank restarted under a new pid" "$seedbank_pid_before" "$seedbank_pid_after"

## The one property this whole design exists for: containers already
## running before the upgrade were never stopped, not even briefly.

traefik_started_at_after="$(ssh_out "docker inspect -f '{{.State.StartedAt}}' doug.traefik")"
assert_equals "traefik container was never restarted during the upgrade" \
    "$traefik_started_at_before" "$traefik_started_at_after"

assert_success "traefik container is still running after the upgrade" ssh_out \
    "docker ps --filter name=doug.traefik --filter status=running -q | grep -q ."

assert_success "clean up the candidate binary" ssh_out "rm -f ~/douglas-new"

finish
