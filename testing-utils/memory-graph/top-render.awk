BEGIN {
    FS = ","
    split("▁ ▂ ▃ ▄ ▅ ▆ ▇ █", block, " ")
    esc = sprintf("%c", 27)
    erase = esc "[K"
    reset = color ? esc "[0m" : ""
    bold = color ? esc "[1m" : ""
    dim = color ? esc "[2m" : ""
    red = color ? esc "[31m" : ""
    tint["process"] = color ? esc "[36m" : ""
    tint["container"] = color ? esc "[32m" : ""
    tint["total"] = color ? esc "[1;37m" : ""
    heading["process"] = "processes"
    heading["container"] = "containers"
    rank["process"] = 1
    rank["container"] = 2
}

{
    tick = $1 + 0
    id = $3 "/" $2
    if (tick > last) last = tick
    if (!(id in group)) {
        group[id] = $3
        name[id] = $2
        ids[++count] = id
    }
    value[id, tick] = $4 + 0
    if ($4 + 0 > peak[id]) peak[id] = $4 + 0
    sum[tick] += $4 + 0
    if (sum[tick] > sum_peak) sum_peak = sum[tick]
}

function out(text) {
    printf "%s%s\n", text, erase
}

function human(amount) {
    if (amount >= 1073741824) return sprintf("%.2f GiB", amount / 1073741824)
    if (amount >= 1048576) return sprintf("%.1f MiB", amount / 1048576)
    if (amount >= 1024) return sprintf("%.0f KiB", amount / 1024)
    return sprintf("%d B", amount)
}

function duration(seconds) {
    if (seconds >= 60) return sprintf("%dm%02ds", int(seconds / 60), seconds % 60)
    return sprintf("%ds", seconds)
}

function before(left, right) {
    if (rank[group[left]] != rank[group[right]]) return rank[group[left]] < rank[group[right]]
    return name[left] < name[right]
}

function sort_ids(    i, j, held) {
    for (i = 2; i <= count; i++) {
        held = ids[i]
        for (j = i - 1; j >= 1 && before(held, ids[j]); j--) ids[j + 1] = ids[j]
        ids[j + 1] = held
    }
}

function load_series(id,    tick_index) {
    delete current
    for (tick_index = first; tick_index <= last; tick_index++)
        if ((id, tick_index) in value) current[tick_index] = value[id, tick_index]
}

function load_total(    tick_index) {
    delete current
    for (tick_index = first; tick_index <= last; tick_index++)
        if (tick_index in sum) current[tick_index] = sum[tick_index]
}

function window_max(    tick_index, result) {
    result = 0
    for (tick_index = first; tick_index <= last; tick_index++)
        if (tick_index in current && current[tick_index] > result) result = current[tick_index]
    return result
}

function spark(global_max,    tick_index, sample, low, high, span, level, text) {
    low = -1
    high = 0
    for (tick_index = first; tick_index <= last; tick_index++) {
        if (!(tick_index in current)) continue
        sample = current[tick_index]
        if (low < 0 || sample < low) low = sample
        if (sample > high) high = sample
    }
    if (mode == "zero") low = 0
    if (mode == "global") {
        low = 0
        high = global_max
    }
    span = high - low
    if (mode == "range" && span < high * 0.1) span = high * 0.1
    if (span <= 0) span = 1
    text = ""
    for (tick_index = first; tick_index <= last; tick_index++) {
        if (!(tick_index in current)) {
            text = text " "
            continue
        }
        level = 1 + int((current[tick_index] - low) / span * 7 + 0.5)
        if (level < 1) level = 1
        if (level > 8) level = 8
        text = text block[level]
    }
    return text
}

function row(label, kind, graph, now_text, peak_text) {
    return sprintf(name_format "  %s%s%s  %10s  %10s", substr(label, 1, name_width), tint[kind], graph, reset, now_text, peak_text)
}

function series_row(id) {
    load_series(id)
    return row(name[id], group[id], spark(global_max), ((id, last) in value) ? human(value[id, last]) : "-", human(peak[id]))
}

function total_row() {
    load_total()
    return row("total", "total", spark(sum_peak), (last in sum) ? human(sum[last]) : "-", human(sum_peak))
}

function header(    span_text, gap) {
    out(bold "douglas memory" reset dim "  " vm "  every " interval "s  scale " mode "  " count " series" reset)
    span_text = "-" duration((graph_width - 1) * interval)
    gap = graph_width - length(span_text) - 3
    if (gap < 1) gap = 1
    out(dim sprintf(name_format "  %s%" gap "s%s  %10s  %10s", "", span_text, "", "now", "current", "peak") reset)
}

function footer() {
    out(dim "q quit  s scale (" mode ")  " reset (failed ? red : dim) status reset)
}

function body(    i, previous) {
    previous = ""
    for (i = 1; i <= count; i++) {
        if (group[ids[i]] != previous) {
            previous = group[ids[i]]
            body_lines[++body_count] = bold heading[previous] reset
        }
        body_lines[++body_count] = series_row(ids[i])
    }
}

END {
    if (count == 0) {
        name_width = 8
        name_format = "%-" name_width "s"
        graph_width = 20
        header()
        out("waiting for douglas processes or containers...")
        footer()
        exit
    }
    sort_ids()
    name_width = 8
    for (i = 1; i <= count; i++)
        if (length(name[ids[i]]) > name_width) name_width = length(name[ids[i]])
    if (name_width > 24) name_width = 24
    name_format = "%-" name_width "s"
    graph_width = cols - name_width - 26
    if (graph_width < 10) graph_width = 10
    if (graph_width > 400) graph_width = 400
    first = last - graph_width + 1

    for (i = 1; i <= count; i++) {
        load_series(ids[i])
        if (window_max() > global_max) global_max = window_max()
    }

    header()
    out(total_row())
    body()
    available = lines - 1 - 4
    shown = body_count
    if (body_count > available) shown = available - 1
    for (i = 1; i <= shown; i++) out(body_lines[i])
    if (body_count > available) out(dim "... " (body_count - shown) " more rows (enlarge the terminal)" reset)
    footer()
}
