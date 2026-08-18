# Shared helpers for smoke test steps. Sourced by each script in steps/
# (and by run.sh, which just executes each step as its own process).
#
# Not meant to be run directly.

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && cd .. && pwd)"
SSH_KEY="${DOUGLAS_SMOKE_SSH_KEY:-$REPO_ROOT/testing-utils/nix/ssh-keys/douglas_id_ed25519}"
VM="${DOUGLAS_SMOKE_VM:-dev@douglas-dev.local}"
FAILURES=0

BOLD_GREEN=$'\033[1;32m'
BOLD_RED=$'\033[1;31m'
RESET=$'\033[0m'

# pass/fail <description> — the shared "  PASS: ...\"/\"  FAIL: ...\" line
# every assert_* helper below prints, colored and bolded.
pass() {
    echo "  ${BOLD_GREEN}PASS${RESET}: $1"
}

fail() {
    echo "  ${BOLD_RED}FAIL${RESET}: $1"
}

# ssh_out <remote command...>
# Runs the remote command and strips this VM's `~/.bashrc` startup banner
# (the "🚀 Douglas Development Environment" block, echoed unconditionally on
# every ssh exec, not just interactive logins) from stdout, so callers that
# parse the output (assert_owner_group_mode, log_line_count) get just the
# real payload.
#
# LogLevel=ERROR suppresses the pre-auth SSH banner from configuration.nix's
# `services.openssh.settings.banner` (nice for a human logging in by hand,
# just noise across dozens of automated calls) while still surfacing real
# ssh-level errors (auth failure, connection refused, etc.) — unlike -q,
# which silences those too. Only affects invocations from this test
# harness; interactive `ssh` from a terminal is untouched.
ssh_out() {
    local status
    ssh -o LogLevel=ERROR -i "$SSH_KEY" "$VM" "$@" | grep -vE '^(🚀|📁|📦|🔧)|^$'
    status="${PIPESTATUS[0]}"
    return "$status"
}

# assert_success <description> <command...>
# Runs the command; failure prints its captured output for debugging.
assert_success() {
    local desc="$1"
    shift
    local output
    if output="$("$@" 2>&1)"; then
        pass "$desc"
    else
        fail "$desc"
        echo "$output" | sed 's/^/    /'
        FAILURES=$((FAILURES + 1))
    fi
}

# assert_failure <description> <command...>
# Asserts the command exits non-zero (e.g. "this container should be gone").
assert_failure() {
    local desc="$1"
    shift
    if "$@" >/dev/null 2>&1; then
        fail "$desc (expected failure, command succeeded)"
        FAILURES=$((FAILURES + 1))
    else
        pass "$desc"
    fi
}

# assert_contains <description> <haystack> <needle>
assert_contains() {
    local desc="$1" haystack="$2" needle="$3"
    if [[ "$haystack" == *"$needle"* ]]; then
        pass "$desc"
    else
        fail "$desc (expected to contain '$needle', got: $haystack)"
        FAILURES=$((FAILURES + 1))
    fi
}

# assert_owner_group_mode <description> <remote path> <expected "user:group:mode">
# Runs `stat` via sudo: these paths live under service-owned directories
# (e.g. /var/lib/douglas/mounts/traefik/...) that the unprivileged `dev`
# user isn't expected to be able to traverse — narrowly-scoped service
# groups like douglas-traefik are deliberately not dev's groups. Checking
# ownership/mode is an introspection task, not a "can dev use this"
# capability check, so it should always succeed via sudo regardless of
# ancestor directory permissions.
assert_owner_group_mode() {
    local desc="$1" path="$2" expected="$3"
    local actual
    actual="$(ssh_out sudo stat -c '%U:%G:%a' "$path")"
    if [ "$actual" = "$expected" ]; then
        pass "$desc"
    else
        fail "$desc (expected $expected, got $actual)"
        FAILURES=$((FAILURES + 1))
    fi
}

# assert_no_log_errors <description> <log path> <line count before the step>
assert_no_log_errors() {
    local desc="$1" log_path="$2" marker="$3"
    local new_lines
    new_lines="$(ssh_out tail -n "+$((marker + 1))" "$log_path")"
    if echo "$new_lines" | grep -Eq 'level=(Warn|Error)'; then
        fail "$desc"
        echo "$new_lines" | grep -E 'level=(Warn|Error)' | sed 's/^/    /'
        FAILURES=$((FAILURES + 1))
    else
        pass "$desc"
    fi
}

# wait_until <description> <timeout seconds> <command...>
# Polls the command every 1s until it succeeds or the timeout elapses, then
# reports pass/fail the same way the assert_* helpers do. For conditions
# that only become true asynchronously — e.g. bract's push-triggered
# reconcile, which builds/starts a container and writes its traefik route
# off the reconcile-trigger notification rather than synchronously with the
# `docker push` that kicks it off.
wait_until() {
    local desc="$1" timeout="$2"
    shift 2
    local deadline=$((SECONDS + timeout))
    until "$@" >/dev/null 2>&1; do
        if [ "$SECONDS" -ge "$deadline" ]; then
            fail "$desc (timed out after ${timeout}s)"
            FAILURES=$((FAILURES + 1))
            return
        fi
        sleep 1
    done
    pass "$desc"
}

# log_line_count <remote log path> — captured before a step, passed to
# assert_no_log_errors afterward so only newly-appended lines are checked.
# The `wc -l < path` redirection must happen on the remote shell, hence
# passing it as a single command string rather than separate ssh arguments.
log_line_count() {
    ssh_out "wc -l < '$1' 2>/dev/null || echo 0"
}

finish() {
    exit "$FAILURES"
}
