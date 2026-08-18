# Smoke tests

Happy-path end-to-end checks for the douglas CLI, run against a real dev VM
(NixOS live-CD). Each step is a small bash script that runs a douglas command
over SSH and asserts on the result — exit code, output, remote file
permissions, or log contents.

These are not unit tests and are not run by `cargo test`. They're meant to be
run by hand against your dev VM whenever you want to confirm the full
lifecycle still works end to end.

## Running

```bash
./run.sh
```

By default this connects as `dev@douglas-dev.local` using the key checked in
at `testing-utils/nix/ssh-keys/douglas_id_ed25519`. Override either with:

```bash
DOUGLAS_SMOKE_VM=dev@my-host DOUGLAS_SMOKE_SSH_KEY=/path/to/key ./run.sh
```

The first step reboots the VM back to the live-CD's pristine state before
anything else runs, and waits for SSH to come back up (up to
`DOUGLAS_SMOKE_REBOOT_TIMEOUT` seconds, default 120). Skip it with
`DOUGLAS_SMOKE_SKIP_REBOOT=1` if you're iterating quickly against a VM you
already know is clean.

`run.sh` executes every script in `steps/`, in numeric-prefix order, and
**stops at the first step that fails.** This is a linear lifecycle, not a set
of independent checks — `seedling status` has nothing to report if `seedling
new` never ran — so once a step fails, every later step would just cascade
into further, misleading failures. Output looks like:

```
=== steps/10-start.sh ===
  PASS: douglas start exits successfully
  PASS: douglas-resin user exists
  ...
  PASS: traefik container is running
=== steps/20-seedling-new.sh ===
  ...
----
all steps passed
```

or, on a failure:

```
=== steps/30-seedling-status.sh ===
  FAIL: status reports the seedling is running after push
    ...
----
FAILED at steps/30-seedling-status.sh — stopping (later steps assume this one passed)
```

To iterate on a single step without re-running the whole suite:

```bash
./steps/20-seedling-new.sh
```

## What's covered

The steps walk through the CLI's full happy path, in order:

| Step | Covers |
| --- | --- |
| `00-reboot.sh` | (setup only — reboots the VM to a clean live-CD state and waits for SSH) |
| `05-build.sh` | (setup only — builds douglas from the shared checkout and deploys it to `~/douglas` on the VM) |
| `10-start.sh` | `douglas start` — comprehensive: every service user/group, every folder + socket `start` is responsible for (exact owner:group:mode), and that bract/resin/seedbank are actually running (process + socket/port liveness), not just that the command exited 0 |
| `20-seedling-new.sh` | `douglas seedling new` — comprehensive: every file/dir seedbank writes for the new seedling (exact owner:group:mode + content), and negative checks confirming reconcile *hasn't* run yet (no rolodex account, no docker network/container) |
| `25-push-image.sh` | (setup only — pushes an image so later steps have something to operate on; also verifies reconcile's docker network and that the seedling is actually reachable through traefik with HTTP 200 + a real response body) |
| `30-seedling-status.sh` | `douglas seedling status` |
| `40-seedling-stop.sh` | `douglas seedling stop` — also confirms traefik no longer routes to it (HTTP non-200), not just that the container object reports stopped |
| `50-seedling-start.sh` | `douglas seedling start` — also confirms traefik routes to it again with HTTP 200 + a real response body |
| `60-seedling-drop.sh` | `douglas seedling drop` — comprehensive: reverses every check from `20-seedling-new.sh` (every seedbank file/dir gone) and `25-push-image.sh` (docker network, traefik route file, and HTTP reachability all gone) |
| `70-seedling-create-template.sh` | `douglas seedling create-template` |
| `80-seedling-prune.sh` | `douglas seedling prune` |
| `90-stop.sh` | `douglas stop` (currently expected to fail — see below) |

`90-stop.sh` asserts `douglas stop` *fails*, because the command is still a
`todo!()` in `src/main.rs`. Once it's implemented, flip that step to
`assert_success` and update its comment.

## Keeping this in sync with the CLI

Every step script declares which CLI command it exercises via a comment near
the top:

```bash
# covers: seedling status
```

A unit test, `command_coverage_tests::test_smoke_test_steps_should_cover_every_non_hidden_cli_command`
in `src/main.rs`, walks the CLI's actual command tree (via clap) and asserts
it matches the set of `# covers:` lines across all step scripts. If you add,
rename, or remove a CLI command without updating a step to match, `cargo
test` fails with a diff telling you exactly what's out of sync.

So: whenever you touch the CLI's command surface, either add/update a step
script with the right `# covers:` line, or update an existing one — the test
won't let you forget.

## Writing a new step

- Name it `<NN>-<description>.sh` — the numeric prefix controls run order.
- Start with `# covers: <command path>` (e.g. `# covers: seedling drop`),
  unless the step is setup-only and doesn't itself map to a CLI command (see
  `25-push-image.sh`).
- `source ../lib.sh` for the assertion helpers (`assert_success`,
  `assert_failure`, `assert_contains`, `assert_owner_group_mode`,
  `assert_no_log_errors`) and call `finish` at the end.
- Prefer asserting on real state (file ownership/permissions, log contents,
  container status) over just the command's exit code — several bugs this
  suite is meant to catch (misconfigured directory permissions, silent
  no-op reconciles) wouldn't show up in an exit code alone.
- When shelling out to the VM, pass the *entire* remote command — including
  any pipe — as a single quoted string to `ssh_out`, e.g.
  `ssh_out "docker ps --filter name=foo -q | grep -q ."`. Splitting a piped
  command across multiple `ssh_out` arguments breaks where the pipe binds.

## Requirements

- SSH access to the target VM as `dev@douglas-dev.local` using
  `testing-utils/nix/ssh-keys/douglas_id_ed25519` (or an equivalent host/key
  passed via `DOUGLAS_SMOKE_VM`/`DOUGLAS_SMOKE_SSH_KEY`).
- `00-reboot.sh` puts the VM in a known-good state automatically (it's a
  live-CD, so a reboot returns it to a pristine boot). The remaining steps
  mutate real state (create/drop seedlings, push images) and aren't designed
  to be idempotent or self-cleaning, so they rely on that clean start.
- This repo checked out under `/mnt/share/douglas` on the host, visible to
  the VM via UTM's shared-folder mount — `05-build.sh` builds directly from
  that path and deploys the resulting binary to `~/douglas`, mirroring the
  manual build-and-test workflow.
- Commands run against the VM invoke the deployed binary as `~/douglas`
  rather than a bare `douglas`, since the VM's non-interactive `ssh` sessions
  don't put it on `$PATH`. Keep new steps consistent with that.
