#!/usr/bin/env bash
set -uo pipefail

bold=$(tput bold)
italic=$(tput sitm)
pink=$(tput setaf 201)
reset=$(tput sgr0)

debug_prefix=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --dbg)
      debug_prefix="rust-lldb"
      shift
      ;;
    --help|-h)
      echo "usage: $0 [--dbg]"
      exit 0
      ;;
    *)
      echo "unknown argument: $1" >&2
      exit 1
      ;;
  esac
done

echo "${italic}${pink}Cleaning up docker containers…${reset}"
declare -a containers=(
    "openbao"
    "coredns"
    "traefik"
    "who-am-i"
)
for container in "${containers[@]}"
do
    docker stop $container
    docker rm $container
done

echo "${italic}${pink}Cleaning up docker networks…${reset}"
declare -a networkss=(
    "douglas-system"
    "douglas-development-infrastructure"
)
for network in "${networks[@]}"
do
    docker network rm $network
done

echo "${italic}${pink}Cleaning up mounts…${reset}"
sudo rm -rf /var/lib/douglas

echo "${italic}${pink}Cleaning up logs…${reset}"
sudo rm -rf /var/log/douglas

echo "${italic}${pink}Cleaning up config…${reset}"
sudo rm -rf /etc/douglas

echo "${italic}${pink}Force killing bract…${reset}"
sudo kill -9 `sudo cat /run/douglas/bract.pid`

echo "${bold}${pink}we ball${reset}"

# RUSTFLAGS=-Awarnings
cargo build \
&& cp ./target/debug/douglas ~/ \
&& sudo -E $debug_prefix ~/douglas start \
&& echo "${italic}${pink}snagging cert…${reset}" \
&& sudo cp /var/lib/douglas/mounts/traefik/certificates/current/root-ca.pem ~/root-ca.pem \
&& sudo chown $(whoami) ~/root-ca.pem
