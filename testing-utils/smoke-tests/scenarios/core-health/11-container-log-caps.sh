#!/usr/bin/env bash
set -uo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"
source ../../lib.sh

container_id="$(ssh_out "docker ps --filter name=doug.traefik --filter status=running -q")"
assert_success "traefik container is running" test -n "$container_id"

log_config="$(ssh_out "docker inspect --format '{{json .HostConfig.LogConfig}}' '$container_id'")"

assert_contains "traefik container caps log size at 10m" "$log_config" '"max-size":"10m"'
assert_contains "traefik container caps rotated log files at 5" "$log_config" '"max-file":"5"'

finish
