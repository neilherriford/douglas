BEGIN { FS = "," ; n = split(order, names, " ") }
$1 == "timestamp_utc" { next }
$2 != prev_elapsed && prev_elapsed != "" { flush() }
{ prev_elapsed = $2; val[$4] = $5 }
END { flush() }
function flush() {
    line = prev_elapsed
    cum = 0
    for (i = 1; i <= n; i++) {
        v = (names[i] in val) ? val[names[i]] : 0
        cum += v
        line = line "," (cum / 1048576)
    }
    print line
    delete val
}
