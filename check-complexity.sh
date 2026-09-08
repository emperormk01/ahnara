#!/bin/bash
# Rust cyclomatic complexity linter (pure bash/awk, no external deps)
# Counts complexity markers: if/match/while/for/loop, &&, ||, ?
# Reports functions with complexity > 10

set -euo pipefail

THRESHOLD=${1:-10}

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
find "$SCRIPT_DIR/src" -name "*.rs" -print0 | while IFS= read -r -d '' file; do
    awk -v threshold="$THRESHOLD" -v file="$file" '
    BEGIN {
        in_func = 0
        func_name = ""
        func_start = 0
        brace_depth = 0
        func_open_brace = 0
        complexity = 0
    }

    function count_token(line, token, tlen,    tmp, n) {
        tmp = line
        gsub(token, "", tmp)
        n = (length(line) - length(tmp)) / tlen
        return n
    }

    function count_complexity(line,    tmp, n) {
        n = 0
        tmp = line
        gsub(/"[^"]*"/, "", tmp)
        gsub(/\x27[^\x27]*\x27/, "", tmp)
        sub(/\/\/.*/, "", tmp)
        n += count_token(tmp, "if", 2)
        n += count_token(tmp, "match", 5)
        n += count_token(tmp, "while", 5)
        n += count_token(tmp, "for", 3)
        n += count_token(tmp, "loop", 4)
        return n
    }

    function count_logical_ops(line,    tmp, t, n) {
        n = 0
        tmp = line
        gsub(/"[^"]*"/, "", tmp)
        gsub(/\x27[^\x27]*\x27/, "", tmp)
        sub(/\/\/.*/, "", tmp)
        t = tmp; gsub(/&&/, "", t); n = n + (length(tmp) - length(t)) / 2
        t = tmp; gsub(/\|\|/, "", t); n = n + (length(tmp) - length(t)) / 2
        return n
    }

    function count_question_marks(line,    tmp, t, n) {
        n = 0
        tmp = line
        gsub(/"[^"]*"/, "", tmp)
        gsub(/\x27[^\x27]*\x27/, "", tmp)
        gsub(/r#"[^"]*"#/, "", tmp)
        sub(/\/\/.*/, "", tmp)
        gsub(/\?\?/, "", tmp)
        gsub(/\?=/, "", tmp)
        gsub(/\?\./, "", tmp)
        gsub(/\)\?/, "", tmp)
        gsub(/\]\?/, "", tmp)
        t = tmp; gsub(/\?/, "", t); n = length(tmp) - length(t)
        return n
    }

    function report_violation() {
        if (complexity > threshold) {
            printf "%s:%d: function \"%s\" has complexity %d (threshold %d)\n", file, func_start, func_name, complexity, threshold
            return 1
        }
        return 0
    }

    function finish_func() {
        if (in_func) {
            if (report_violation()) exit 1
            in_func = 0
        }
    }

    {
        # Count braces on this line (remove strings/comments first)
        stripped = $0
        gsub(/"[^"]*"/, "", stripped)
        gsub(/\/\/.*/, "", stripped)
        gsub(/\x27[^\x27]*\x27/, "", stripped)

        num_open = 0
        tmp = stripped
        while (match(tmp, /\{/)) { num_open = num_open + 1; tmp = substr(tmp, RSTART + 1) }
        num_close = 0
        tmp = stripped
        while (match(tmp, /\}/)) { num_close = num_close + 1; tmp = substr(tmp, RSTART + 1) }

        # If we are inside a function and we see closing braces that take us
        # back to or past the function open brace level, the function ends here
        if (in_func) {
            # Check if any closing brace on this line closes the function
            net_after = brace_depth + num_open - num_close
            if (net_after < func_open_brace || (net_after == func_open_brace && num_close > 0)) {
                # Count complexity for this final line BEFORE closing
                complexity += count_complexity($0)
                complexity += count_logical_ops($0)
                complexity += count_question_marks($0)
                if (report_violation()) exit 1
                in_func = 0
                # Update depth and continue (remaining braces belong to outer scope)
                brace_depth = net_after
            } else {
                brace_depth = net_after
                complexity += count_complexity($0)
                complexity += count_logical_ops($0)
                complexity += count_question_marks($0)
            }
        } else {
            brace_depth = brace_depth + num_open - num_close

            # Detect function start (only when not already in a function)
            if (match($0, /^[[:space:]]*(pub[[:space:]]+)?(async[[:space:]]+)?(unsafe[[:space:]]+)?(extern[[:space:]]+"[^"]*"[[:space:]]+)?fn[[:space:]]+[a-zA-Z_]/)) {
                in_func = 1
                match($0, /fn[[:space:]]+[a-zA-Z_][a-zA-Z0-9_]*/)
                func_name = substr($0, RSTART + 3, RLENGTH - 3)
                gsub(/^[[:space:]]+/, "", func_name)
                gsub(/[[:space:]]+$/, "", func_name)
                func_start = NR
                func_open_brace = brace_depth
                complexity = 0

                # If function opens AND closes on same line, count and finish
                if (num_close > 0 && brace_depth < func_open_brace) {
                    complexity += count_complexity($0)
                    complexity += count_logical_ops($0)
                    complexity += count_question_marks($0)
                    if (report_violation()) exit 1
                    in_func = 0
                }
            }
        }
    }

    END {
        if (in_func) {
            if (report_violation()) exit 1
        }
    }
    ' "$file"
done

exit 0
