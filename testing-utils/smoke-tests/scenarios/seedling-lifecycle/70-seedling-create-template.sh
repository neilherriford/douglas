#!/usr/bin/env bash
# covers: seedling create-template
set -uo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"
source ../../lib.sh

template="$(ssh_out '~/douglas' seedling create-template)"
assert_contains "template includes a mounts section" "$template" "[mounts"
assert_contains "template includes a ports section" "$template" "[ports]"
assert_contains "default template routes at a subdomain" "$template" 'route = "subdomain"'
assert_contains "default template uses a local image" "$template" 'type = "local"'

root_template="$(ssh_out '~/douglas' seedling create-template --root)"
assert_contains "--root template routes at the root" "$root_template" 'route = "root"'
assert_contains "--root template still uses a local image" "$root_template" 'type = "local"'

foreign_template="$(ssh_out '~/douglas' seedling create-template --template-style=foreign-image)"
assert_contains "foreign-image template uses an external image" "$foreign_template" 'type = "external"'
assert_contains "foreign-image template names its upstream reference" "$foreign_template" "reference = \"docker.io/"
assert_contains "foreign-image template routes at a subdomain" "$foreign_template" 'route = "subdomain"'

foreign_root_template="$(ssh_out '~/douglas' seedling create-template --template-style=foreign-image --root)"
assert_contains "foreign-image --root template routes at the root" "$foreign_root_template" 'route = "root"'
assert_contains "foreign-image --root template uses an external image" "$foreign_root_template" 'type = "external"'

assert_failure "an unknown template style is rejected" ssh_out \
    "~/douglas seedling create-template --template-style=bogus"

finish
