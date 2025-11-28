BEGIN {
    # Assign ANSI codes to variables for readability
    BOLD="\x1b[1m"; RESET="\x1b[0m"
    GREEN="\x1b[1;32m"; YELLOW="\x1b[1;33m"; RED="\x1b[1;31m"
}

# Rule 1: Handle HTTP status lines
/^HTTP\// {
    if ($2 ~ /^[12]/) print GREEN $0 RESET;
    else if ($2 ~ /^3/) print YELLOW $0 RESET;
    else print RED $0 RESET;
    next # Crucial: skip to the next line of input
}

# Rule 2: Handle lines with a colon (headers)
/:/ {
    sub(/:/, RESET "&"); # Insert RESET code right before the first colon
    print BOLD $0;
    next
}

# Rule 3: Default for all other lines
{ print BOLD $0 RESET }

