BEGIN { FS = "," }
$1 == "timestamp_utc" { next }
$4 == name { count++; values[count] = $5; sum += $5 }
END {
    if (count == 0) { print "0 0"; exit }
    for (i = 1; i <= count; i++)
        for (j = i + 1; j <= count; j++)
            if (values[j] < values[i]) {
                tmp = values[i]; values[i] = values[j]; values[j] = tmp
            }
    if (count % 2 == 1) median = values[(count + 1) / 2]
    else median = (values[count / 2] + values[count / 2 + 1]) / 2
    printf "%.1f %.1f\n", sum / count / 1048576, median / 1048576
}
