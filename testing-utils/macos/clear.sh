#!/bin/bash
echo "deleting mounts"
sudo rm -rf /tmp/mounts/*

echo "deleting doug users"
dscl . -list /Users | grep '^doug-' | xargs -I% sudo dscl . -delete /Users/%

echo "deleting doug groups"
dscl . -list /Groups | grep -E '^(doug-|douglas)' | xargs -I% sudo dscl . -delete /Groups/%
