BEGIN {
    mult["B"] = 1
    mult["kB"] = 1000
    mult["MB"] = 1000000
    mult["GB"] = 1000000000
    mult["KiB"] = 1024
    mult["MiB"] = 1048576
    mult["GiB"] = 1073741824
}

function bytes(text,    number, unit) {
    number = text
    sub(/[A-Za-z]+$/, "", number)
    unit = text
    sub(/^[0-9.]+/, "", unit)
    return number * mult[unit]
}

function container_label(container) {
    if (container ~ /^doug-agent\./) {
        sub(/^doug-agent\./, "", container)
        return container "-agent"
    }
    sub(/^doug\./, "", container)
    return container
}

$1 == "P" {
    printf "%d,%s,process,%.0f\n", tick, $2, $3 * 1024
}

$1 == "C" {
    printf "%d,%s,container,%.0f\n", tick, container_label($2), bytes($3)
}
