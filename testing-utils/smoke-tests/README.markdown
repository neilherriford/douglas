# Smoke tests and the CI platform

End-to-end checks for the douglas CLI and the system it runs, executed against
real, disposable qemu VMs. `./ci-fanout.sh` builds douglas once, boots one
fresh headless VM per scenario, runs every scenario in parallel, and leaves an
audit trail. These are not unit tests and are not run by `cargo test`.

## What it is for

Unit tests use mocks, so they can't see the failures that only appear when the
real pieces meet: a real kernel, real users and file permissions, real sockets,
a real Docker daemon, real processes being killed. This suite runs douglas as
a user would, on a clean machine, and asserts on real state (owners and modes,
listening sockets, running processes and containers, HTTP responses, log
contents), not just exit codes.

It exists to discover things like:

- **Setup and permissions bugs:** wrong owners or modes, missing users, groups,
  folders or sockets after `start`.
- **Reconcile and lifecycle bugs:** seedlings that don't come up, don't route
  through traefik, or leave things behind when stopped, dropped or pruned.
  Several real bugs were found this way: a blob uploader writing to the wrong
  path, `resin` delete requiring a live registration, `seedling start`
  breaking external images, a hardcoded OpenBao image path, and deadwood
  pruning cached upstream repos.
- **Races and load behavior:** the fan-out runs many VMs at once, which exposed
  a race between the reconcile and bract's watchdog both starting the same
  container (Docker answers the second start with `304`, which was treated as
  an error).
- **Recovery paths:** upgrade and rollback failing halfway, an unhealthy new
  version, a frozen bract during `stop`, woodward giving up on a service, a
  seedling that can never pass its health check.
- **Resource problems:** the guests are deliberately 2 CPU / 2GB, so memory
  regressions and out-of-memory kills show up here.

## Quick start

```bash
./ci-fanout.sh                                             # everything, in parallel
./ci-fanout.sh --only upgrade-rollback,woodward-gives-up   # just these scenarios
./ci-fanout.sh --refresh-cache                             # re-seed resin's image cache
./ci-fanout.sh --report                                    # print the latest run's SUMMARY.md
./ci-fanout.sh --clean                                     # stop leftover VMs, delete their disks
```

The whole suite takes about four minutes. It is bounded by the slowest
scenario (`watchdog-stops-degrading-seedling`, which waits on real 30s
watchdog sweeps).

### One-time setup on a Mac

- Apple Silicon, with `qemu` from Homebrew under `/opt/homebrew` (HVF
  acceleration), `jq`, `cargo-zigbuild`, and the rustup target
  `aarch64-unknown-linux-musl`.
- `testing-utils/nix/douglas-ci.iso` (see "Rebuilding the CI image").
- The signing key `keys/douglas_signing.key` (gitignored).
- `testing-utils/nix/ssh-keys/` (gitignored; the key the guests trust).

`QEMU_BIN`, `QEMU_ACCEL` and `QEMU_BIOS` can be overridden for another host. An
aarch64 Linux host with KVM should work but is untested; an x86_64 host would
need an x86_64 nix output and musl target that don't exist yet.

There is deliberately no default VM: scenarios kill, upgrade and stop douglas,
so `lib.sh` refuses to run unless `DOUGLAS_SMOKE_VM` is set. The interactive
UTM dev VM is not a smoke-test target.

## Feedback while it runs, and the audit trail

Every lane phase (booting, syncing, restoring the cache) and every finished
step is printed as it happens, with its check count, and a heartbeat every 30s
lists what each running scenario is doing.

Every run also writes `testing-utils/smoke-tests/runs/<run id>/` (gitignored;
the newest 10 are kept, and `runs/latest` points at the last one):

| File | What it is |
| --- | --- |
| `SUMMARY.md` | Paste-able report: the exact tree tested, artifact hashes, host, a results table, and every failed check with how to reach the VM |
| `run.json` | The same, machine-readable: per-scenario per-step results (status, seconds, checks passed/failed) and every failed assertion |
| `<scenario>.log` | That scenario's full transcript |
| `<scenario>.guest-diagnostics.log` | Guest logs (douglas, bract, resin, seedbank, woodward, docker, OOM events); failed scenarios only |

The tree is identified by git commit, branch, the number of uncommitted files,
and a hash of the uncommitted changes, so a report says exactly what was tested
even when nothing is committed. When reporting a problem, send `SUMMARY.md`
plus `run.json` and the failing scenario's two logs.

## When a scenario fails

The VM of a failed scenario is **left running**, with its disks, so you can
look around. The summary prints, per failed scenario, the failed checks and how
to reach the VM (`DOUGLAS_SMOKE_VM=douglas-lane-<name>`, plus a raw `ssh`
line). To re-run part of a scenario against that VM by hand:

```bash
DOUGLAS_SMOKE_VM=douglas-lane-upgrade-rollback \
DOUGLAS_SMOKE_SSH_KEY=../nix/ssh-keys/douglas_id_ed25519 \
    ./run-scenario.sh upgrade-rollback --no-setup --only 85,86
```

Nothing is cleaned up until you run `./ci-fanout.sh --clean`, which stops every
leftover VM, deletes their disks and scratch logs, and removes the temporary
ssh aliases (audit trails are kept). A normal run refuses to start while
leftover VMs exist, because they hold the ports. Scenarios that pass clean up
after themselves.

## How it works

```
Host (your Mac)
  cargo zigbuild + xtask sign     one static, signed, debug douglas binary per run
  ci-fanout.sh                    one qemu VM per scenario, all in parallel
    qemu (HVF, 2 vCPU, 2GB)       douglas-ci.iso (read-only) + two throwaway disks
      /var/lib/douglas            disk #1: retained binaries, resin cache, docker data
      /home                       disk #2: ~/douglas and upgrade candidates
  resin-cache/*.tar               image cache seeded once, restored into every VM

Guest (NixOS live image, no toolchain)
  run-scenario.sh                 setup/ then the scenario's numbered steps, over ssh
```

- **One binary, built once.** The guest has no Rust toolchain. Anything needing
  a signed binary (the deploy, upgrade candidates) is signed on the host:
  `lib.sh`'s `host_sign` copies the file down, runs `xtask sign`, and copies it
  back. It is a full debug build (`debug_assert!`, overflow checks, symbols).
- **Nothing that grows lives in RAM.** The live image's root is a tmpfs capped
  at half of RAM (971MB on the 2GB guest) and counts against memory, so two
  sparse disk images are attached to each VM and auto-formatted ext4 on first
  boot. Docker's `data-root` points inside `/var/lib/douglas`. Both disks are
  deleted when the scenario passes and kept with the VM when it fails.
- **Seeded resin cache.** Resin caches images by digest, so a populated cache
  means no blob downloads. `ci-fanout.sh` seeds it once from a real VM
  (`seed-resin-cache.sh`) into `testing-utils/nix/resin-cache/` (gitignored) and
  restores it into every VM before `douglas start`. Tags are still resolved
  upstream on every pull (a cheap HEAD), so mutable tags stay fresh.
  `--refresh-cache` rebuilds it.
- **Pristine every time.** A VM is created per scenario and thrown away, so no
  scenario can affect another and no state carries over between runs.

## Layout

```
setup/            run before every scenario, in order
  05-deploy.sh      copy the shared, pre-signed binary to ~/douglas
  10-start.sh       `douglas start`, with exhaustive state checks
scenarios/<name>/ one directory = one scenario = one VM
  NN-*.sh           steps, run in lexical order; the first failure stops the scenario
lib.sh            assertion helpers shared by every script
run-scenario.sh   runs setup/ then one scenario's steps against DOUGLAS_SMOKE_VM
ci-fanout.sh      builds once, then runs every scenario in parallel on fresh VMs
seed-resin-cache.sh   one-off: fills the host-side resin cache from Docker Hub
runs/             audit trails (gitignored)
```

Steps inside a scenario share state and build on each other (a seedling
created by an early step is still there for the later ones), which is why a
scenario is the unit of parallelism, not a step. A new directory under
`scenarios/` is picked up automatically as a new lane.

| Scenario | Covers |
| --- | --- |
| `core-health` | `verify`; every container carries the docker log-driver cap; woodward heals a killed service; the install marker and upgrade journal; bract's own log rotation; `status` |
| `audit-log-rotation` | bract's periodic sweep rotating openbao's real audit log past 10MB, with no warnings or errors |
| `broken-log-mount` | the same sweep degrading gracefully when openbao's log mount is broken, and recovering once it is restored |
| `seedling-lifecycle` | `seedling new`, push, routing through traefik, log rotation, subdomains, external (Docker Hub) images, `status`, secrets, `stop`, `start`, `drop`, `create-template`, `prune` |
| `upgrade-rollback` | `upgrade` (invalid, unhealthy, failing-to-start, one-way, real) and `rollback` (planned, real, one-way), then `stop` |
| `woodward-gives-up` | a support process fails every kick and woodward gives up supervising it |
| `seedling-exhausts-health-checks` | a seedling whose health check never passes is stopped for good after 5 failures |
| `watchdog-stops-degrading-seedling` | the same give-up, reached by watchdog's own sweeps with no CLI calls (about 4 minutes of real waiting) |
| `stop-with-frozen-bract` | `stop` escalating when bract is frozen |
| `start-right-after-a-kill-restarts-woodward` | regression: `start` right after a kill must bring woodward back |
| `upgrade-from-another-filesystem` | `upgrade` from a candidate on a different filesystem than the install target |

## Extending it

**Add a check to an existing step:** use the helpers from `lib.sh`
(`assert_success`, `assert_failure`, `assert_contains`, `assert_equals`,
`assert_not_equals`, `assert_non_empty`, `assert_owner_group_mode`,
`assert_no_log_errors`, `wait_until`). Prefer asserting on real state (file
ownership/permissions, log contents, container status, HTTP responses) over just
an exit code.

**Add a step to a scenario:** drop a `NN-description.sh` into
`scenarios/<name>/`; the numeric prefix orders it. Template:

```bash
#!/usr/bin/env bash
# covers: seedling drop
set -uo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"
source ../../lib.sh

assert_success "the thing worked" ssh_out "~/douglas seedling status --name foo"

finish
```

**Add a scenario:** create `scenarios/<new-name>/` with one or more step scripts.
It runs on its own fresh VM with `setup/` already done (binary deployed,
`douglas start` complete), and the next `./ci-fanout.sh` picks it up as a new lane
with no other edits. Make it a new scenario, not a step, when it drives the system
into a state nothing later can recover from (a service given up on, a wrecked
install) or takes long enough that it should run in parallel with the rest.

**Conventions**
- Start with `# covers: <command path>` (e.g. `# covers: seedling drop`) unless
  the script is setup-only. A unit test,
  `command_coverage_tests::test_smoke_test_scripts_should_cover_every_non_hidden_cli_command`
  in `src/cli.rs`, walks the CLI's command tree via clap and fails `cargo test`
  with a diff if a command has no covering script.
- Pass the *entire* remote command, including any pipe, as one quoted string to
  `ssh_out`: `ssh_out "docker ps --filter name=foo -q | grep -q ."`.
- Invoke the deployed binary as `~/douglas`; non-interactive `ssh` sessions don't
  have it on `$PATH`.
- Anything that needs a signed binary (an upgrade candidate) uses
  `build_upgrade_candidate` / `build_stub_upgrade_candidate` / `host_sign`; the
  guest cannot sign.
- Don't assume timing. Bract's watchdog sweeps every 30s and its health rechecks
  change failure counts; `docker stop` has a 10s grace. Synchronize on a
  completed sweep or an observed state (see `seedling-exhausts-health-checks`),
  and remember the suite runs many VMs at once, so things are slower under load.
- Only `example-seedlings/` is copied to the VM (to `~/example-seedlings`, exposed
  to scripts as `$EXAMPLE_SEEDLINGS`) so steps can `docker build` them. None of
  the Rust source goes to the guest. Scripts run on the host, so anything else
  from the repo is read there directly, e.g. `cargo_version` reads the host's
  `Cargo.toml`. Spec files piped to `seedling new` also come from the host.

**Change what's on the guest** (packages, disks, services): edit
`testing-utils/nix/ci-configuration.nix`, then rebuild the image.

**Change VM size or lane behavior:** the qemu flags (`-m`, `-smp`, disk sizes)
and the lane loop are in `ci-fanout.sh`. Keep the 2 CPU / 2GB sizing unless
you are deliberately testing something else, since it is what surfaces memory
problems.

**Refresh the resin cache** when the core images (traefik, openbao) change
versions: `./ci-fanout.sh --refresh-cache`. A stale cache is harmless; an image
missing from it just falls back to a normal pull.

## Rebuilding the CI image

The image is built on the nix build VM (no local nix install needed). From
`testing-utils/nix/`, with `BUILD_VM` set to the build host and its key:

```bash
rsync -az -e "ssh -i $BUILD_KEY" ci-configuration.nix flake.nix "$BUILD_VM:~/nix/"
ssh -i "$BUILD_KEY" "$BUILD_VM" '. /etc/profile.d/nix.sh && cd ~/nix && nix build .#ci --max-jobs 5'
rsync -a --checksum -e "ssh -i $BUILD_KEY" "$BUILD_VM:~/nix/result/iso/*.iso" douglas-ci.iso
```

The ISO is large and untracked. Delete the old `douglas-ci.iso` first if it is
read-only (nix store outputs are), and compare `shasum -a 256` on both sides.
The build VM's ssh key is separate from the guest key in `ssh-keys/`.

## Troubleshooting

- **"VMs from a previous run are still up":** a failed scenario left its VM
  running on purpose. Inspect it, then `./ci-fanout.sh --clean`.
- **A VM "never comes up":** check the ssh host-key state. Each lane uses its own
  `known_hosts` file precisely because reusing `127.0.0.1:port` across VMs makes
  ssh fail verification, which looks like a hang if errors are hidden.
- **`Permission denied` reaching the build VM:** it uses its own key
  (`douglas_build_id_ed25519`); load it into the ssh agent or pass `-i`.
- **Out-of-memory or "no space left" in a guest:** everything that grows should
  be on the two disks; look at `df` and `free` in the kept VM, and at the
  `oom` section of the guest diagnostics.
- **Flakes under load:** the suite runs about a dozen VMs at once. Prefer
  synchronizing on observed state over sleeps, and check the guest diagnostics
  before assuming a rig problem.
