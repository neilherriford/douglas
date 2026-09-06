#!/usr/bin/env bash
# covers: start
#
# Comprehensive check of what `douglas start` is supposed to set up: the
# system users/groups it creates, every folder + socket it owns (with the
# exact owner:group:mode each one is bootstrapped with — see
# bract/src/blueprints/bootstrap.rs, resin/src/bootstrap.rs,
# seedbank/src/bootstrap.rs, and config/src/lib.rs's DouglasFolders for
# where these paths and modes come from), and that bract/resin/seedbank are
# actually running as processes and reachable over their sockets/port —
# not just that the command exited 0.
set -uo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"
source ../lib.sh

assert_success "douglas start exits successfully" ssh_out \
    "sudo ~/douglas --output-style plain start"

## Users and groups (bract/resin/seedbank's service accounts)

assert_success "douglas-resin user exists" ssh_out "getent passwd douglas-resin"
assert_success "douglas-seedbank user exists" ssh_out "getent passwd douglas-seedbank"

assert_success "douglas-admin group exists" ssh_out "getent group douglas-admin"
assert_success "douglas-resin group exists" ssh_out "getent group douglas-resin"
assert_success "douglas-seedbank group exists" ssh_out "getent group douglas-seedbank"
assert_success "douglas-resin-bract group exists" ssh_out "getent group douglas-resin-bract"
assert_success "douglas-resin-seedbank group exists" ssh_out "getent group douglas-resin-seedbank"

## Folders and their exact owner:group:mode
#
# "path|owner:group:mode|description" — one row per folder `start` is
# responsible for creating. Owners/modes here are as declared in each
# service's `service_definition()`, cross-checked directly against the
# running VM while writing this.
FOLDER_CHECKS=(
    "/var/log/douglas|root:douglas-admin:2771|douglas log root"
    "/run/douglas|root:douglas-admin:771|douglas transients root"
    "/var/lib/douglas|root:douglas-admin:771|douglas applications root"
    "/var/lib/douglas/services|root:douglas-admin:770|application services dir"
    "/var/lib/douglas/mounts|root:douglas-admin:770|application mounts dir"
    "/etc/douglas|root:douglas-admin:2770|douglas configs dir"
    "/var/lib/douglas/rolodex|root:douglas-admin:2770|rolodex dir"
    "/var/lib/douglas/credentials|root:douglas-admin:700|credentials dir"
    "/var/lib/douglas-identity|root:douglas-admin:700|identity dir"
    "/run/douglas/bract|root:douglas-admin:770|bract socket dir"
    "/run/douglas/bract-trigger|root:douglas-resin-bract:770|bract-trigger socket dir"
    "/run/douglas/bract-heartbeat|root:douglas-admin:770|bract heartbeat dir"
    "/var/log/douglas/bract|root:douglas-admin:770|bract log dir"
    "/var/lib/douglas/resin/repositories|douglas-resin:douglas-resin:770|resin repositories dir"
    "/var/log/douglas/resin|douglas-resin:douglas-resin:770|resin log dir"
    "/run/douglas/resin-heartbeat|douglas-resin:douglas-resin:771|resin heartbeat dir"
    "/var/lib/douglas/seedbank/seeds|douglas-seedbank:douglas-seedbank:770|seedbank seeds dir"
    "/run/douglas/seedbank|douglas-seedbank:douglas-seedbank:771|seedbank socket dir"
    "/run/douglas/seedbank-registration|douglas-seedbank:douglas-seedbank:771|seedbank-registration socket dir"
    "/run/douglas/seedbank-heartbeat|douglas-seedbank:douglas-seedbank:771|seedbank heartbeat dir"
    "/var/log/douglas/seedbank|douglas-seedbank:douglas-seedbank:770|seedbank log dir"
)

for entry in "${FOLDER_CHECKS[@]}"; do
    IFS='|' read -r path expected desc <<<"$entry"
    assert_owner_group_mode "$desc" "$path" "$expected"
done

## Sockets: exist, with the right owner:group:mode, and are actually
## listening (not just present as a stale file from a previous run)
SOCKET_CHECKS=(
    "/run/douglas/bract/bract.sock|root:douglas-admin:660|bract control socket"
    "/run/douglas/bract-trigger/bract-trigger.sock|root:douglas-resin-bract:660|bract-trigger socket"
    "/run/douglas/seedbank/seedbank.sock|douglas-seedbank:douglas-resin-seedbank:660|seedbank control socket"
    "/run/douglas/seedbank-registration/seedbank-registration.sock|douglas-seedbank:douglas-resin-seedbank:660|seedbank-registration socket"
)

for entry in "${SOCKET_CHECKS[@]}"; do
    IFS='|' read -r path expected desc <<<"$entry"
    assert_owner_group_mode "$desc" "$path" "$expected"
    assert_success "$desc is listening" ssh_out "sudo ss -xlp | grep -q '$path'"
done

## Services are actually running — as processes, not just "the sockets
## exist" (a crashed process can leave its socket file behind).
# `[s]ervice` (not `service`) keeps pgrep from matching its own command line.
assert_success "bract process is running" ssh_out "pgrep -f '[s]ervice bract'"
assert_success "resin process is running" ssh_out "pgrep -f '[s]ervice resin'"
assert_success "seedbank process is running" ssh_out "pgrep -f '[s]ervice seedbank'"

# resin has no unix socket (it's an HTTP registry on 127.0.0.1:7376, not a
# unix-socket service like bract/seedbank) — confirm it actually answers
# rather than just that something bound the port.
assert_success "resin responds on its registry API" ssh_out \
    "curl -sf http://localhost:7376/v2/ >/dev/null"

## Heartbeats: each service periodically stamps a liveness file inside its
## own dedicated heartbeat dir (see DouglasFolders::heartbeat_dir /
## service_heartbeat_file and woodward::HeartbeatWriter) so an external
## supervisor can tell a hung process from a healthy one, not just a
## crashed one from a running one. It needs its own directory rather than
## living flat under /run/douglas itself — resin and seedbank run as their
## own unprivileged service accounts, which have no write access to
## /run/douglas (root:douglas-admin, mode 771); each service is instead
## granted ownership of its own <name>-heartbeat directory to write into.
## Confirming the file's content actually changes catches a heartbeat loop
## that wrote once at startup and then silently stopped ticking — a
## process-exists check alone would never notice that.
HEARTBEAT_SERVICES=(bract resin seedbank)

for service in "${HEARTBEAT_SERVICES[@]}"; do
    heartbeat_path="/run/douglas/${service}-heartbeat/heartbeat"

    assert_success "$service heartbeat file exists" ssh_out \
        "sudo test -f '$heartbeat_path'"

    initial_content="$(ssh_out sudo cat "$heartbeat_path")"
    assert_contains "$service heartbeat file records a written_at timestamp" \
        "$initial_content" "written_at"

    heartbeat_has_advanced() {
        [ "$(ssh_out sudo cat "$heartbeat_path")" != "$initial_content" ]
    }

    wait_until "$service heartbeat updates within one interval" 15 heartbeat_has_advanced
done

## traefik (the one seedling bract manages itself, brought up as part of
## `start`, not through `seedling new`/push like hello-world)

# "doug-traefik-858bd5f5" is the deterministic per-seedling service account
# name rolodex derives for "traefik" (bract/src/rolodex/naming.rs:
# "doug-" + normalized name, truncated, + first 4 bytes of sha256(name) as
# hex) — stable across reboots, not a random per-instance value.
assert_owner_group_mode "traefik dynamic routes dir owned by traefik service account" \
    "/var/lib/douglas/mounts/traefik/config/dynamic" \
    "doug-traefik-858bd5f5:doug-traefik-858bd5f5:2770"

assert_success "traefik container is running" ssh_out \
    "docker ps --filter name=doug.traefik --filter status=running -q | grep -q ."

finish
