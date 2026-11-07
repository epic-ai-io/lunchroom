#!/bin/sh
# tee-wrap.sh LABEL COMMAND [ARGUMENT ...]
#
# Runs a Codex hook command and keeps, exactly, what Codex gave it on stdin and what it printed on
# stdout and stderr and its exit code, in the folder named in /run/sess/hookdir; then prints the same
# stdout and stderr and exits with the same code, so Codex sees what it would have seen without the
# wrapper (apart from the extra process).
label=$1
shift
dir=$(cat /run/sess/hookdir 2>/dev/null || echo /run/sess/hooks)
mkdir -p "$dir"
n=$(date +%s%N)
base="$dir/$n-$label"
cat > "$base.in"
"$@" < "$base.in" > "$base.out" 2> "$base.err"
code=$?
echo "$code" > "$base.code"
cat "$base.out"
cat "$base.err" >&2
exit "$code"
