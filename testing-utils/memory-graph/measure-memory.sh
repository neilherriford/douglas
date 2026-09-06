#!/usr/bin/env bash
# Provisions a full douglas system, built in release (production) mode, then
# samples RSS/cgroup memory for the core services and every seedling
# container over a fixed window, writing a tidy (long-format) CSV:
#
#   timestamp_utc,elapsed_seconds,category,name,rss_bytes
#
# Sampling starts right after the release binary is deployed but *before*
# `douglas start` runs, and continues concurrently through the whole
# startup sequence (bract/resin/seedbank spawning, core seedlings
# reconciling, hello-world/secrets coming up) into steady state — so the
# CSV captures the actual allocation ramp/spikiness at startup, not just
# the settled-down state afterward. Elapsed 0 is "about to start douglas,"
# not "provisioning already finished."
#
# One row per (sample tick, entity) rather than one column per entity, so
# adding a new core service or seedling later is just one more array entry
# below, not a CSV schema migration. See plot-memory.gnuplot for a starter
# gnuplot script that filters this format by `name`.
#
# Lives alongside (not inside) testing-utils/smoke-tests/ — it isn't part of
# the pass/fail smoke suite, but reuses that suite's VM/build/provisioning
# steps for everything short of the actual build (which needs --release
# here, not the smoke suite's debug build).
#
# woodward itself isn't a row here: today it's a library bract/resin/
# seedbank each link in and run as an internal task, not a standalone
# process — its overhead already shows up inside their own RSS. Once M1's
# external heartbeat reader/restarter exists as its own process, add it to
# CORE_PROCESSES below like any other host process.
#
# Usage:
#   ./measure-memory.sh                              # full provision + 5min sample, starting pre-launch
#   DURATION_SECONDS=1800 ./measure-memory.sh         # 30min sample window (still starts pre-launch)
#   INTERVAL_SECONDS=2 ./measure-memory.sh            # finer-grained sampling
#   SKIP_PROVISION=1 ./measure-memory.sh              # system already up + seeded, just sample
#   SKIP_REBOOT=1 ./measure-memory.sh                 # provision without a VM reboot first
#   CLEANUP_AFTER=1 ./measure-memory.sh               # drop hello-world/secrets when done
#   OUTPUT_CSV=/tmp/run1.csv ./measure-memory.sh
#   SKIP_PLOT=1 ./measure-memory.sh                   # CSV only, no gnuplot/PNG step
#   NO_OPEN=1 ./measure-memory.sh                     # render the PNG but don't open a viewer
#
# Renders memory-usage-<timestamp>.png alongside the CSV via plot-memory.gnuplot
# and opens it (macOS `open`/Linux `xdg-open`) when done, unless SKIP_PLOT/NO_OPEN
# say otherwise. Both the CSV and the PNG are gitignored — regenerate anytime.
set -uo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"
SMOKE_TESTS_DIR="../smoke-tests"
source "$SMOKE_TESTS_DIR/lib.sh"

DURATION_SECONDS="${DURATION_SECONDS:-300}"
INTERVAL_SECONDS="${INTERVAL_SECONDS:-5}"
SKIP_PROVISION="${SKIP_PROVISION:-0}"
SKIP_REBOOT="${SKIP_REBOOT:-0}"
CLEANUP_AFTER="${CLEANUP_AFTER:-0}"
SKIP_PLOT="${SKIP_PLOT:-0}"
NO_OPEN="${NO_OPEN:-0}"
RUN_STAMP="$(date -u +%Y%m%dT%H%M%SZ)"
OUTPUT_CSV="${OUTPUT_CSV:-memory-usage-$RUN_STAMP.csv}"
OUTPUT_PNG="${OUTPUT_PNG:-${OUTPUT_CSV%.csv}.png}"

# "process-pattern-suffix:series-name" — matched via `pgrep -f "[s]ervice <suffix>"`,
# the same pattern 10-start.sh already uses to confirm each is running.
CORE_PROCESSES=(
    "bract:bract"
    "resin:resin"
    "seedbank:seedbank"
)

# "container-name:series-name" — core containers first, then every seedling
# currently provisioned. Add a line here for each new turnkey/user seedling
# as more get exercised (Valkey, Postgres, etc. once M6/M9 land).
CONTAINERS=(
    "doug.traefik:traefik"
    "doug.openbao:openbao"
    "doug.hello-world:hello-world"
    "doug.secrets:secrets"
    "doug-agent.secrets:secrets-agent"
)

build_release() {
    ssh_out "sudo pkill -x douglas" >/dev/null 2>&1 || true
    assert_success "build douglas (release)" ssh_out \
        "cd /mnt/share/douglas && cargo build --release"
    assert_success "deploy douglas binary to home directory" ssh_out \
        "cp /mnt/share/cache/target/release/douglas ~/douglas"
}

# Reboot + build + deploy only — everything up to but not including `douglas
# start`. Runs before sampling begins, so build/compile time itself isn't
# sampled (nothing douglas-related exists yet to measure); the startup ramp
# sampling *is* meant to capture starts right after this returns.
provision_pre_start() {
    if [ "$SKIP_REBOOT" != "1" ]; then
        bash "$SMOKE_TESTS_DIR/steps/00-reboot.sh" || exit 1
    fi
    build_release
}

# `douglas start` through seedling provisioning — run concurrently with the
# sampling loop (see main flow at the bottom) so the CSV captures the
# startup allocation ramp: bract/resin/seedbank spawning, core seedlings
# reconciling, hello-world/secrets coming up, all as it happens rather than
# only once everything has already settled into steady state.
provision_post_start() {
    bash "$SMOKE_TESTS_DIR/steps/10-start.sh" || return 1

    bash "$SMOKE_TESTS_DIR/steps/20-seedling-new.sh" || return 1
    bash "$SMOKE_TESTS_DIR/steps/25-push-image.sh" || return 1

    echo "Provisioning secrets seedling (create-only, no teardown)..."
    ssh -o LogLevel=ERROR -i "$SSH_KEY" "$VM" "~/douglas seedling new --name secrets" \
        <"$REPO_ROOT/example-seedlings/secrets/default.toml" >/dev/null
    assert_success "build secrets image" ssh_out \
        "cd /mnt/share/douglas/example-seedlings/secrets && docker build . --tag secrets"
    assert_success "tag secrets image for the local registry" ssh_out \
        "docker tag secrets localhost:7376/secrets"
    assert_success "push secrets image" ssh_out \
        "docker push localhost:7376/secrets"
    wait_until "secrets reconcile finished (traefik route file appeared)" 30 ssh_out \
        "sudo test -f /var/lib/douglas/mounts/traefik/config/dynamic/secrets.yml"
    wait_until "secrets' agent container is running" 15 ssh_out \
        "docker inspect --format '{{.State.Running}}' doug-agent.secrets | grep -q true"

    if [ "$FAILURES" -ne 0 ]; then
        echo "Provisioning failed ($FAILURES failure(s))."
        return 1
    fi
}

# Bytes for a docker-style size like "12.5MiB" or "512B" — always base-1024,
# matching what `docker stats`'s --format actually prints. Uses bash's own
# regex matching rather than gawk's 3-arg match(), since this runs locally
# and macOS's stock /usr/bin/awk doesn't support that extension.
to_bytes() {
    local value="$1"
    if [[ "$value" =~ ^([0-9.]+)([A-Za-z]+)$ ]]; then
        local n="${BASH_REMATCH[1]}" unit="${BASH_REMATCH[2]}" mult
        case "$unit" in
        B) mult=1 ;;
        KiB) mult=1024 ;;
        MiB) mult=$((1024 * 1024)) ;;
        GiB) mult=$((1024 * 1024 * 1024)) ;;
        TiB) mult=$((1024 * 1024 * 1024 * 1024)) ;;
        *) mult=0 ;;
        esac
        awk -v n="$n" -v m="$mult" 'BEGIN { printf "%.0f\n", n * m }'
    else
        echo 0
    fi
}

sample_processes() {
    local remote_script='
for suffix in bract resin seedbank; do
    pid=$(pgrep -f "[s]ervice $suffix" | head -1)
    if [ -n "$pid" ]; then
        rss_kb=$(ps -o rss= -p "$pid" 2>/dev/null | tr -d " ")
    else
        rss_kb=""
    fi
    echo "$suffix:${rss_kb:-0}"
done'
    ssh_out bash -c "'$remote_script'"
}

sample_containers() {
    local names=()
    local entry
    for entry in "${CONTAINERS[@]}"; do
        names+=("${entry%%:*}")
    done
    local names_str="${names[*]}"

    # `docker stats` with any nonexistent name in the list fails outright —
    # no output at all, even for names that do exist. Real during the
    # pre-launch ramp: containers come up one at a time, so most ticks have
    # a mix of existing and not-yet-created names. Filter to what actually
    # exists first, remotely, in the same round trip.
    local remote_script="
tracked=\"$names_str\"
existing=\$(docker ps --format \"{{.Names}}\")
to_check=\"\"
for name in \$tracked; do
    if echo \"\$existing\" | grep -qx \"\$name\"; then
        to_check=\"\$to_check \$name\"
    fi
done
if [ -n \"\$to_check\" ]; then
    docker stats --no-stream --format \"{{.Name}},{{.MemUsage}}\" \$to_check
fi
"
    ssh_out bash -c "'$remote_script'"
}

measure() {
    : >"$OUTPUT_CSV"
    echo "timestamp_utc,elapsed_seconds,category,name,rss_bytes" >>"$OUTPUT_CSV"

    local samples=$((DURATION_SECONDS / INTERVAL_SECONDS))
    local start_time=$SECONDS
    local i

    echo "Sampling every ${INTERVAL_SECONDS}s for ${DURATION_SECONDS}s (${samples} samples) -> $OUTPUT_CSV"

    for ((i = 0; i < samples; i++)); do
        local tick_started=$SECONDS
        local timestamp elapsed
        timestamp="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
        elapsed=$((SECONDS - start_time))

        local proc_lines container_lines
        proc_lines="$(sample_processes)"
        container_lines="$(sample_containers)"

        local entry suffix series rss_kb
        for entry in "${CORE_PROCESSES[@]}"; do
            suffix="${entry%%:*}"
            series="${entry##*:}"
            rss_kb="$(echo "$proc_lines" | awk -F: -v s="$suffix" '$1==s {print $2}')"
            echo "$timestamp,$elapsed,core,$series,$(( ${rss_kb:-0} * 1024 ))" >>"$OUTPUT_CSV"
        done

        local container_name mem_field bytes
        for entry in "${CONTAINERS[@]}"; do
            container_name="${entry%%:*}"
            series="${entry##*:}"
            mem_field="$(echo "$container_lines" | awk -F, -v n="$container_name" '$1==n {print $2}' | awk -F/ '{print $1}' | tr -d ' ')"
            if [ -n "$mem_field" ]; then
                bytes="$(to_bytes "$mem_field")"
            else
                bytes=0
            fi
            local category="seedling"
            [[ "$container_name" == "doug.traefik" || "$container_name" == "doug.openbao" ]] && category="core"
            echo "$timestamp,$elapsed,$category,$series,$bytes" >>"$OUTPUT_CSV"
        done

        printf '.'

        local tick_elapsed=$((SECONDS - tick_started))
        local remaining=$((INTERVAL_SECONDS - tick_elapsed))
        [ "$remaining" -gt 0 ] && sleep "$remaining"
    done

    echo
    echo "Done. Wrote $OUTPUT_CSV"
}

cleanup() {
    [ "$CLEANUP_AFTER" != "1" ] && return
    echo "Cleaning up hello-world and secrets seedlings..."
    ssh_out "~/douglas seedling stop --name secrets" >/dev/null 2>&1 || true
    ssh_out "~/douglas seedling drop --name secrets" >/dev/null 2>&1 || true
    ssh_out "~/douglas seedling stop --name hello-world" >/dev/null 2>&1 || true
    ssh_out "~/douglas seedling drop --name hello-world" >/dev/null 2>&1 || true
}

plot() {
    [ "$SKIP_PLOT" = "1" ] && return
    if ! command -v gnuplot >/dev/null 2>&1; then
        echo "gnuplot not found — skipping PNG render (CSV is still at $OUTPUT_CSV)"
        return
    fi
    if ! gnuplot -e "csv='$OUTPUT_CSV'; out='$OUTPUT_PNG'" plot-memory.gnuplot; then
        echo "gnuplot failed — skipping PNG render (CSV is still at $OUTPUT_CSV)"
        return
    fi
    echo "Wrote $OUTPUT_PNG"

    [ "$NO_OPEN" = "1" ] && return
    if command -v open >/dev/null 2>&1; then
        open "$OUTPUT_PNG"
    elif command -v xdg-open >/dev/null 2>&1; then
        xdg-open "$OUTPUT_PNG" >/dev/null 2>&1 &
    fi
}

if [ "$SKIP_PROVISION" != "1" ]; then
    provision_pre_start
fi

measure &
measure_pid=$!

if [ "$SKIP_PROVISION" != "1" ]; then
    if ! provision_post_start; then
        echo "Provisioning failed — stopping sampling early."
        kill "$measure_pid" 2>/dev/null || true
        wait "$measure_pid" 2>/dev/null || true
        exit 1
    fi
fi

wait "$measure_pid"
plot
cleanup
