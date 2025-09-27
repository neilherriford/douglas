#!/bin/bash

echo "Deleting mounts"
if [ -d  /tmp/mounts ]; then
    sudo rm -rf /tmp/mounts/*
    sudo rmdir /tmp/mounts
fi

douglas_groups=$(getent group | cut --delimiter=: --fields=1 | grep --extended-regexp '^(doug-|douglas)')

echo "Removing members from douglas groups..."
for group in $douglas_groups; do
    echo "  • $group"

    members=$(getent group "$group" | cut -d: -f4)
    if [[ -n "$members" ]]; then
        IFS=',' read -ra users <<< "$members"
        for user in "${users[@]}"; do
            echo "    ∙ removing $user from $group"
            sudo gpasswd --delete "$user" "$group"
        done
    fi
done

echo "Deleting douglas users…"
getent passwd | cut --delimiter=: --fields=1 | grep --extended-regexp '^doug-' | while read -r user; do
    echo "  • $user"
    sudo userdel "$user"
done

echo "Delete douglas groups…"
getent group | cut --delimiter=: --fields=1 | grep --extended-regexp '^(doug-|douglas)' | while read -r group; do
    echo "  • $group"

    echo "      removing members"
    members=$(getent group "$group" | cut --delimiter=: --fields=4)
    if [[ -n "$members" ]]; then
        IFS=',' read -ra users <<< "$members"
        for user in "${users[@]}"; do
            echo "        $user"
            sudo gpasswd --delete "$user" "$group"
        done
    fi

    sudo groupdel "$group"
done





    # echo "    setting primary group to 'nogroup'"
    # sudo usermod --gid nogroup "$user"

    # echo "    removing all supplementary groups"
    # sudo usermod --groups "" "$user"

    # echo "    delete user"
    # sudo userdel "$user"

# echo "reassigning douglas users primary group to 'nogroup'"
# getent passwd | cut --delimiter=: --fields=1 | grep '^doug-' | while read user; do
#     echo "  • $user"
#     sudo usermod --gid nogroup "$user"
# done

# echo "deleting groups"
# getent group | grep -E '^(doug-|douglas)' | cut -d: -f1 | while read group; do
#   echo "Cleaning up group: $group"
#   members=$(getent group "$group" | cut -d: -f4)
#   for member in $(echo "$members" | tr ',' ' '); do
#     echo " - Removing $member from $group"
#     sudo gpasswd -d "$member" "$group" || true
#   done
#   echo " - Deleting group $group"
#   sudo groupdel "$group"
# done

# echo "deleting doug users"
# getent passwd | cut --delimiter=: --fields=1 | grep '^doug-' | while read user; do
#     echo "  • $user"
#     sudo userdel "$user"
# done
