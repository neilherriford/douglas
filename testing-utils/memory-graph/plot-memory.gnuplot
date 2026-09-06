# Stacked, filled memory-over-time chart in the style of macOS Activity
# Monitor's Memory tab — each service's RSS is drawn as a colored band, and
# the total stack height at any point in time is the sum of every band.
# Each band's legend entry is labeled with its average and median RSS over
# the whole sampled window.
#
# Reads the tidy (long-format) CSV from measure-memory.sh:
#   timestamp_utc,elapsed_seconds,category,name,rss_bytes
# and pivots it via stack-memory.awk into cumulative-sum columns (one per
# series, in stacking order) before plotting — the CSV itself stays
# long-format/append-friendly; only this plot step needs the wide shape.
# series-stats.awk computes each series' own average/median directly from
# the same long CSV, independent of the stacking pivot.
#
# RSS = Resident Set Size: the physical memory each process/container
# currently has mapped in RAM. It can double-count memory shared between
# processes (e.g. shared libraries), so the stack's total is an upper
# bound on real usage, not an exact sum — same caveat Activity Monitor's
# own per-process figures carry.
#
# Usage:
#   gnuplot -e "csv='memory-usage-20260906T190052Z.csv'" plot-memory.gnuplot
#   gnuplot -e "csv='run1.csv'; out='run1.png'" plot-memory.gnuplot   # PNG instead of a window
#
# To track a new service: add its series name to `series` below in the
# order you want it stacked (bottom of the chart = first in the list).

if (!exists("csv")) csv = "memory-usage.csv"

if (exists("out")) {
    set terminal pngcairo size 1280,800 font ",11"
    set output out
}

set datafile separator ","

run_started_at = system("awk -F, 'NR==2 {print $1; exit}' ".csv)
set title "Douglas memory usage over time (stacked RSS)\n{/*0.7 run started ".run_started_at."}"
set xlabel "elapsed seconds"
set ylabel "Resident Set Size — RSS (MiB)"
set key outside right top title "service (avg / median RSS)"
set grid
set style fill solid 0.85 border -1

series = "bract resin seedbank traefik openbao hello-world secrets secrets-agent"
n = words(series)

data = "< awk -f stack-memory.awk -v order='".series."' ".csv

stats_for(name) = system("awk -f series-stats.awk -v name='".name."' ".csv)
label_for(name) = name." (avg ".word(stats_for(name), 1)." / med ".word(stats_for(name), 2)." MiB)"

plot for [i=n:1:-1] data using 1:(column(i + 1)) with filledcurves y1=0 \
    title label_for(word(series, i))
