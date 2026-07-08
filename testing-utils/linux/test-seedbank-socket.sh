#!/usr/bin/env bash
# Send one JSON request to the seedbank control socket and print the response.
#
# Usage:
#   ./test-seedbank-socket.sh                              # List
#   ./test-seedbank-socket.sh '{"type":"Exists","name":"foo"}'
#   ./test-seedbank-socket.sh '{"type":"Create","seedling":{"name":"foo"}}'
#
# Requires read/write access to the socket (root, douglas-seedbank, or a
# member of douglas-resin-seedbank).

set -euo pipefail

SOCKET="${SEEDBANK_SOCKET:-/run/douglas/seedbank/seedbank.sock}"
if [ "$#" -ge 1 ]; then
    REQUEST="$1"
else
    REQUEST='{"type":"List"}'
fi

if [ ! -S "$SOCKET" ]; then
    echo "No socket at $SOCKET" >&2
    exit 1
fi

printf '%s\n' "$REQUEST" | socat - "UNIX-CONNECT:$SOCKET"
