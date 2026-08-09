#!/usr/bin/env bash
# Benchmark every archive type ttarchive can write, against the system tools.
#
#   ./tests/benchmarks/formats.sh [work-dir]
#
# Three tables: creation, extraction, and what each compression level costs.
# Every row reports wall time, peak resident memory and the resulting size, so a
# format that is fast because it barely compresses is visible as such.
#
# Peak memory is sampled from /proc while the child runs, so it needs no
# external tools. Tools that are not installed are reported as such rather than
# failing the run.

set -u

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
WORK="${1:-$ROOT/target/bench-formats}"
TTAR="$ROOT/target/release/examples/ttar"

mkdir -p "$WORK" || exit 1
echo "building the ttar example"
(cd "$ROOT" && cargo build --release --example ttar >/dev/null) || exit 1

have() { command -v "$1" >/dev/null 2>&1; }
sz() { stat -c%s "$1" 2>/dev/null || echo 0; }

CORPUS="$WORK/corpus"
if [ ! -d "$CORPUS" ]; then
    echo "building the corpus"
    mkdir -p "$CORPUS"
    for i in 1 2 3 4 5 6; do cp -r "$ROOT/src" "$CORPUS/copy$i"; done
    head -c 12000000 /dev/urandom > "$CORPUS/noise.bin"
    for i in $(seq 1 300); do printf 'the quick brown fox jumps over the lazy dog %s\n' "$i"; done > "$CORPUS/text.txt"
    for i in $(seq 1 400); do cat "$CORPUS/text.txt"; done > "$CORPUS/text-big.txt"
fi

INPUT=$(du -sb "$CORPUS" | cut -f1)
FILES=$(find "$CORPUS" -type f | wc -l)

# Run a command, capturing wall time in seconds and peak RSS in MB. Results land
# in RUN_SECS and RUN_PEAK.
RUN_SECS=0
RUN_PEAK=0
run() {
    local start end pid peak=0 hwm
    start=${EPOCHREALTIME/./}
    "$@" >/dev/null 2>&1 &
    pid=$!
    while kill -0 "$pid" 2>/dev/null; do
        hwm=$(awk '/VmHWM/{print $2}' "/proc/$pid/status" 2>/dev/null)
        [ -n "$hwm" ] && [ "$hwm" -gt "$peak" ] 2>/dev/null && peak=$hwm
    done
    wait "$pid" 2>/dev/null
    end=${EPOCHREALTIME/./}
    RUN_SECS=$(awk -v a="$start" -v b="$end" 'BEGIN{printf "%.2f", (b-a)/1000000}')
    RUN_PEAK=$((peak / 1024))
}

human() { numfmt --to=iec --format='%.1f' "$1" 2>/dev/null || echo "$1"; }
rate() { awk -v b="$1" -v s="$2" 'BEGIN{ if (s+0 == 0) printf "-"; else printf "%.0f", (b/1048576)/s }'; }
pct() { awk -v a="$1" -v b="$2" 'BEGIN{ printf "%.1f", 100*a/b }'; }

FORMATS=(tar tar.gz tar.bz2 tar.xz tar.zst tar.lzma tar.lz zip)
declare -A TOOL=( [tar]="tar:" [tar.gz]="tar:-z" [tar.bz2]="tar:-j" [tar.xz]="tar:-J" [tar.zst]="tar:--zstd" [tar.lzma]="tar:--lzma" [tar.lz]="tar:--lzip" [zip]="zip:" )

echo
echo "corpus: $(human "$INPUT") in $FILES files, at $(nproc 2>/dev/null || echo '?') cores"
echo
echo "CREATION"
printf '%-10s | %8s %8s %8s %7s | %8s %8s %8s\n' format time peak size ratio "tool" "time" "size"
printf '%s\n' "-----------------------------------------------------------------------------------"

for fmt in "${FORMATS[@]}"; do
    ours="$WORK/ours.$fmt"
    rm -f "$ours"
    run "$TTAR" create "$ours" "$CORPUS"
    o_secs=$RUN_SECS; o_peak=$RUN_PEAK; o_size=$(sz "$ours")

    entry="${TOOL[$fmt]}"; tool="${entry%%:*}"; flag="${entry#*:}"
    theirs="$WORK/theirs.$fmt"; rm -f "$theirs"
    if have "$tool"; then
        if [ "$tool" = "tar" ]; then
            if [ -n "$flag" ]; then run tar "$flag" -cf "$theirs" -C "$CORPUS" .; else run tar -cf "$theirs" -C "$CORPUS" .; fi
        else
            run zip -q -r "$theirs" "$CORPUS"
        fi
        t_secs=$RUN_SECS; t_size=$(sz "$theirs")
    else
        t_secs="-"; t_size=0
    fi

    printf '%-10s | %7ss %6sMB %8s %6s%% | %8s %7ss %8s\n' \
        ".$fmt" "$o_secs" "$o_peak" "$(human "$o_size")" "$(pct "$o_size" "$INPUT")" \
        "$tool" "$t_secs" "$([ "$t_size" -gt 0 ] && human "$t_size" || echo '-')"
done

echo
echo "EXTRACTION"
printf '%-10s | %8s %8s %9s | %8s %8s\n' format time peak "MB/s" "tool" "time"
printf '%s\n' "---------------------------------------------------------------"

for fmt in "${FORMATS[@]}"; do
    ours="$WORK/ours.$fmt"
    [ -f "$ours" ] || continue
    rm -rf "$WORK/o-out"; mkdir -p "$WORK/o-out"
    run "$TTAR" extract "$ours" "$WORK/o-out"
    o_secs=$RUN_SECS; o_peak=$RUN_PEAK

    entry="${TOOL[$fmt]}"; tool="${entry%%:*}"
    theirs="$WORK/theirs.$fmt"
    rm -rf "$WORK/t-out"; mkdir -p "$WORK/t-out"
    if have "$tool" && [ -s "$theirs" ]; then
        if [ "$tool" = "tar" ]; then run tar -xf "$theirs" -C "$WORK/t-out"; else run unzip -qq -o "$theirs" -d "$WORK/t-out"; fi
        t_secs=$RUN_SECS
    else
        t_secs="-"
    fi

    printf '%-10s | %7ss %6sMB %9s | %8s %7ss\n' ".$fmt" "$o_secs" "$o_peak" "$(rate "$INPUT" "$o_secs")" "$tool" "$t_secs"
done

echo
echo "COMPRESSION LEVELS (ours)"
printf '%-10s | %23s | %23s | %23s\n' format "fast" "default" "best"
printf '%s\n' "------------------------------------------------------------------------------------------"

for fmt in tar.gz tar.bz2 tar.xz tar.zst tar.lzma tar.lz zip; do
    printf '%-10s |' ".$fmt"
    for level in fast default best; do
        out="$WORK/lvl-$level.$fmt"; rm -f "$out"
        run "$TTAR" create "$out" "$CORPUS" --level "$level"
        printf ' %8s %6s%% %6ss |' "$(human "$(sz "$out")")" "$(pct "$(sz "$out")" "$INPUT")" "$RUN_SECS"
        rm -f "$out"
    done
    printf '\n'
done

echo
echo "SPARSE FILES"
SPARSE="$WORK/sparse"; rm -rf "$SPARSE"; mkdir -p "$SPARSE"
python3 - "$SPARSE/holes.bin" <<'PY'
import sys
with open(sys.argv[1], 'wb') as f:
    f.write(b'head')
    f.seek(64 * 1024 * 1024)
    f.write(b'tail')
PY
rm -f "$WORK/dense.tar" "$WORK/sparse.tar" "$WORK/gnu-sparse.tar"
run "$TTAR" create "$WORK/dense.tar" "$SPARSE"
printf '  %-22s %10s\n' "ours, no --sparse" "$(human "$(sz "$WORK/dense.tar")")"
run "$TTAR" create "$WORK/sparse.tar" "$SPARSE" --sparse
printf '  %-22s %10s  in %ss\n' "ours, --sparse" "$(human "$(sz "$WORK/sparse.tar")")" "$RUN_SECS"
if have tar; then
    run tar --sparse -cf "$WORK/gnu-sparse.tar" -C "$SPARSE" .
    printf '  %-22s %10s  in %ss\n' "GNU tar --sparse" "$(human "$(sz "$WORK/gnu-sparse.tar")")" "$RUN_SECS"
fi

echo
echo "work kept in $WORK"
