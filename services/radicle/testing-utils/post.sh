#!/usr/bin/env bash
set -euo pipefail

bold=$(tput bold)
italic=$(tput sitm)
reset=$(tput sgr0)

if [[ $# -ne 2 ]]; then
  echo "Usage: $0 /path/to/socket payload" >&2
  exit 1
fi

socket="$1"
payload="$2"

# Calculate byte length
length=$(printf "%s" "$payload" | wc -c)

# Convert length to 4-byte big-endian prefix
length_prefix=$(printf "%08x" "$length" | sed 's/../\\x&/g')

# Frame the message and send it
{
  printf "$length_prefix"
  printf "%s" "$payload"
} | socat UNIX-CONNECT:"$socket" - | (
  # Read the first 4 bytes as a length header
  len_bytes=$(dd bs=1 count=4 2>/dev/null | od -An -t x1 | tr -d ' \n')

  # Reconstruct big-endian hex manually
  hex_size="${len_bytes:0:2}${len_bytes:2:2}${len_bytes:4:2}${len_bytes:6:2}"
  dec_size=$((16#$hex_size))
  echo "${italic}Received message:${reset} ${bold}${dec_size}${reset}${italic} bytes${reset}" >&2

  dd bs=1 count="$dec_size" 2>/dev/null
) | jq
