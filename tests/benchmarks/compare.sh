#!/usr/bin/env bash
# Benchmark ttarchive against other ZIP implementations on identical inputs.
#
#   ./benchmarks/compare.sh [work-dir]
#
# Builds the `ttar` example first, generates three corpora, then times every
# tool on every mode it supports. Tools that are not installed are skipped.
#
# Notes on fairness:
#   * ttarchive is measured twice: `ttar` uses all cores, `ttar-1t` is pinned to
#     one thread so it can be compared with the single-threaded tools.
#   * Info-ZIP `zip` and CPython `zipfile` are single-threaded by design.
#   * 7-Zip uses multiple threads where it can.
#   * Compression levels are each tool's default unless a row says otherwise.
#   * Caches are dropped between phases only insofar as `sync` allows without
#     root, so treat small differences as noise.

set -u

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORK="${1:-$ROOT/target/bench}"
TTAR="$ROOT/target/release/examples/ttar"

mkdir -p "$WORK"
cd "$WORK" || exit 1

have() { command -v "$1" >/dev/null 2>&1; }

# Wall-clock seconds for a command, or "-" if it fails.
timeit() {
    local start end
    start=$(date +%s.%N)
    if ! "$@" >/dev/null 2>&1; then
        echo "-"
        return
    fi
    sync
    end=$(date +%s.%N)
    awk -v a="$start" -v b="$end" 'BEGIN { printf "%.2f", b - a }'
}

# Print a row: label then one timing per tool.
row() { printf '%-28s %10s %10s %10s %10s %10s\n' "$@"; }

size_of() { stat -c %s "$1" 2>/dev/null || echo 0; }

# ---------------------------------------------------------------------------
# Corpora
# ---------------------------------------------------------------------------

echo "generating corpora in $WORK ..."

if [ ! -d corpus-text ]; then
    mkdir -p corpus-text
    # ~1 GiB of compressible text.
    yes "the quick brown fox jumps over the lazy dog and keeps on running" \
        | head -c $((1024 * 1024 * 1024)) > corpus-text/text.txt
fi

if [ ! -d corpus-noise ]; then
    mkdir -p corpus-noise
    # 512 MiB of incompressible data.
    head -c $((512 * 1024 * 1024)) /dev/urandom > corpus-noise/noise.bin
fi

if [ ! -d corpus-many ]; then
    mkdir -p corpus-many
    # 5000 small mixed files, ~250 MiB total.
    for i in $(seq 0 4999); do
        d="corpus-many/d$((i % 50))"
        mkdir -p "$d"
        if [ $((i % 3)) -eq 0 ]; then
            head -c 51200 /dev/urandom > "$d/f$i.bin"
        else
            yes "line $i of repetitive content for compression testing" \
                | head -c 51200 > "$d/f$i.txt"
        fi
    done
fi

PW='benchmark-password'

echo
row "operation" "ttarchive" "ttar-1t" "zip/unzip" "7z" "bsdtar"
row "---------" "---------" "-------" "---------" "--" "------"

# ---------------------------------------------------------------------------
# Create, deflate
# ---------------------------------------------------------------------------

for corpus in text noise many; do
    rm -f a-*.zip
    t_tt=$(timeit "$TTAR" create a-tt.zip "corpus-$corpus")
    t_t1=$(timeit "$TTAR" create a-t1.zip "corpus-$corpus" --threads 1)
    t_zip="-"; have zip && t_zip=$(timeit zip -q -r a-zip.zip "corpus-$corpus")
    t_7z="-";  have 7z  && t_7z=$(timeit 7z a -tzip -bso0 -bsp0 a-7z.zip "corpus-$corpus")
    t_bt="-";  have bsdtar && t_bt=$(timeit bsdtar -a -c -f a-bt.zip "corpus-$corpus")
    row "create deflate ($corpus)" "$t_tt" "$t_t1" "$t_zip" "$t_7z" "$t_bt"

    printf '%-28s %10s %10s %10s %10s %10s\n' "  archive size (MiB)" \
        "$(( $(size_of a-tt.zip) / 1048576 ))" \
        "$(( $(size_of a-t1.zip) / 1048576 ))" \
        "$(( $(size_of a-zip.zip) / 1048576 ))" \
        "$(( $(size_of a-7z.zip) / 1048576 ))" \
        "$(( $(size_of a-bt.zip) / 1048576 ))"
done

# ---------------------------------------------------------------------------
# Create, stored (no compression) - isolates I/O and framing overhead
# ---------------------------------------------------------------------------

rm -f s-*.zip
t_tt=$(timeit "$TTAR" create s-tt.zip corpus-noise --level store)
t_t1=$(timeit "$TTAR" create s-t1.zip corpus-noise --level store --threads 1)
t_zip="-"; have zip && t_zip=$(timeit zip -q -0 -r s-zip.zip corpus-noise)
t_7z="-";  have 7z  && t_7z=$(timeit 7z a -tzip -mm=Copy -bso0 -bsp0 s-7z.zip corpus-noise)
row "create store (noise)" "$t_tt" "$t_t1" "$t_zip" "$t_7z" "-"

# ---------------------------------------------------------------------------
# Extract
# ---------------------------------------------------------------------------

rm -f x.zip
"$TTAR" create x.zip corpus-text >/dev/null 2>&1
for tool in tt t1 unzip 7z bsdtar; do :; done

rm -rf out-*
t_tt=$(timeit "$TTAR" extract x.zip out-tt)
t_t1=$(timeit "$TTAR" extract x.zip out-t1 --threads 1)
t_uz="-"; have unzip  && t_uz=$(timeit unzip -q -o x.zip -d out-uz)
t_7z="-"; have 7z     && t_7z=$(timeit 7z x -y -bso0 -bsp0 x.zip -oout-7z)
t_bt="-"; have bsdtar && { mkdir -p out-bt; t_bt=$(timeit bsdtar -x -f x.zip -C out-bt); }
row "extract deflate (text)" "$t_tt" "$t_t1" "$t_uz" "$t_7z" "$t_bt"

rm -rf out-*
"$TTAR" create xm.zip corpus-many >/dev/null 2>&1
t_tt=$(timeit "$TTAR" extract xm.zip out-tt)
t_t1=$(timeit "$TTAR" extract xm.zip out-t1 --threads 1)
t_uz="-"; have unzip  && t_uz=$(timeit unzip -q -o xm.zip -d out-uz)
t_7z="-"; have 7z     && t_7z=$(timeit 7z x -y -bso0 -bsp0 xm.zip -oout-7z)
t_bt="-"; have bsdtar && { mkdir -p out-bt; t_bt=$(timeit bsdtar -x -f xm.zip -C out-bt); }
row "extract deflate (many)" "$t_tt" "$t_t1" "$t_uz" "$t_7z" "$t_bt"

# ---------------------------------------------------------------------------
# Encryption
# ---------------------------------------------------------------------------

rm -f e-*.zip
t_tt=$(timeit "$TTAR" create e-tt.zip corpus-text --password "$PW" --encryption aes256)
t_t1=$(timeit "$TTAR" create e-t1.zip corpus-text --password "$PW" --encryption aes256 --threads 1)
t_7z="-"; have 7z && t_7z=$(timeit 7z a -tzip -bso0 -bsp0 -p"$PW" -mem=AES256 e-7z.zip corpus-text)
row "create AES-256 (text)" "$t_tt" "$t_t1" "-" "$t_7z" "-"

rm -rf oute-*
t_tt=$(timeit "$TTAR" extract e-tt.zip oute-tt --password "$PW")
t_t1=$(timeit "$TTAR" extract e-tt.zip oute-t1 --password "$PW" --threads 1)
t_7z="-"; have 7z && t_7z=$(timeit 7z x -y -bso0 -bsp0 -p"$PW" e-tt.zip -ooute-7z)
row "extract AES-256 (text)" "$t_tt" "$t_t1" "-" "$t_7z" "-"

rm -f z-*.zip
t_tt=$(timeit "$TTAR" create z-tt.zip corpus-text --password "$PW" --encryption zipcrypto)
t_t1=$(timeit "$TTAR" create z-t1.zip corpus-text --password "$PW" --encryption zipcrypto --threads 1)
t_zip="-"; have zip && t_zip=$(timeit zip -q -r -P "$PW" z-zip.zip corpus-text)
t_7z="-";  have 7z  && t_7z=$(timeit 7z a -tzip -bso0 -bsp0 -p"$PW" -mem=ZipCrypto z-7z.zip corpus-text)
row "create ZipCrypto (text)" "$t_tt" "$t_t1" "$t_zip" "$t_7z" "-"

rm -rf outz-*
t_tt=$(timeit "$TTAR" extract z-tt.zip outz-tt --password "$PW")
t_t1=$(timeit "$TTAR" extract z-tt.zip outz-t1 --password "$PW" --threads 1)
t_uz="-"; have unzip && t_uz=$(timeit unzip -q -o -P "$PW" z-tt.zip -d outz-uz)
t_7z="-"; have 7z    && t_7z=$(timeit 7z x -y -bso0 -bsp0 -p"$PW" z-tt.zip -ooutz-7z)
row "extract ZipCrypto (text)" "$t_tt" "$t_t1" "$t_uz" "$t_7z" "-"

# ---------------------------------------------------------------------------
# Multi-volume
# ---------------------------------------------------------------------------

rm -f v-*.z*
t_tt=$(timeit "$TTAR" create v-tt.zip corpus-text --volume-size 104857600)
t_zip="-"; have zip && t_zip=$(timeit zip -q -r -s 100m v-zip.zip corpus-text)
t_7z="-";  have 7z  && t_7z=$(timeit 7z a -tzip -bso0 -bsp0 -v100m v-7z.zip corpus-text)
row "create split 100 MiB" "$t_tt" "-" "$t_zip" "$t_7z" "-"

echo
echo "corpora and archives left in $WORK (delete it to reclaim space)"
