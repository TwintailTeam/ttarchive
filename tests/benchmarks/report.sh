#!/usr/bin/env bash
# Full benchmark report: ttarchive against every other archiver installed,
# across every mode it supports. ZIP first, then the 7z sections at the end.
#
#   cargo build --release --examples && ./tests/benchmarks/report.sh
#
# Corpora are generated once into target/bench and reused. Delete that directory
# to reclaim the space (~4 GB including archives).
#
# Fairness notes:
#   * ttarchive is measured at 1 thread and at full parallelism. Info-ZIP `zip`,
#     `unzip` and CPython are single-threaded by design; 7-Zip threads some modes.
#   * Each tool runs at its own default compression level unless stated.
#   * `sync` is called after each run so buffered writes are included.
#   * Timings are wall clock, single run — treat sub-5% differences as noise.

set -u

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
WORK="${1:-$ROOT/target/bench}"
TTAR="$ROOT/target/release/examples/ttar"
PW='benchmark-password'

[ -x "$TTAR" ] || { echo "build first: cargo build --release --examples"; exit 1; }
mkdir -p "$WORK"; cd "$WORK" || exit 1

have() { command -v "$1" >/dev/null 2>&1; }
sz()   { stat -c %s "$1" 2>/dev/null || echo 0; }

t() {
    local s e
    s=$(date +%s.%N)
    "$@" >/dev/null 2>&1
    sync
    e=$(date +%s.%N)
    awk -v a="$s" -v b="$e" 'BEGIN { printf "%.2f", b - a }'
}

RUN_SECS=0
RUN_PEAK=0
tm() {
    local s e pid peak=0 hwm
    s=$(date +%s.%N)
    "$@" >/dev/null 2>&1 &
    pid=$!
    while kill -0 "$pid" 2>/dev/null; do
        hwm=$(awk '/VmHWM/{print $2}' "/proc/$pid/status" 2>/dev/null)
        [ -n "$hwm" ] && [ "$hwm" -gt "$peak" ] 2>/dev/null && peak=$hwm
    done
    wait "$pid" 2>/dev/null
    sync
    e=$(date +%s.%N)
    RUN_SECS=$(awk -v a="$s" -v b="$e" 'BEGIN { printf "%.2f", b - a }')
    RUN_PEAK=$((peak / 1024))
}

hdr()  { printf '\n\033[1m%s\033[0m\n' "$1"; }
rule() { printf '%s\n' "----------------------------------------------------------------------"; }

if [ ! -d corpus-text ]; then
    mkdir -p corpus-text
    yes "the quick brown fox jumps over the lazy dog and keeps on running" \
        | head -c $((1024 * 1024 * 1024)) > corpus-text/text.txt
fi
if [ ! -d corpus-noise ]; then
    mkdir -p corpus-noise
    head -c $((512 * 1024 * 1024)) /dev/urandom > corpus-noise/noise.bin
fi
if [ ! -d corpus-many ]; then
    mkdir -p corpus-many
    for i in $(seq 0 4999); do
        d="corpus-many/d$((i % 50))"; mkdir -p "$d"
        if [ $((i % 3)) -eq 0 ]; then head -c 51200 /dev/urandom > "$d/f$i.bin"
        else yes "line $i of repetitive content for compression testing" | head -c 51200 > "$d/f$i.txt"; fi
    done
fi
if [ ! -d corpus-codec ]; then
    mkdir -p corpus-codec
    yes "the quick brown fox jumps over the lazy dog and keeps on running" | head -c $((24 * 1024 * 1024)) > corpus-codec/text.txt
    head -c $((8 * 1024 * 1024)) /dev/urandom > corpus-codec/noise.bin
    find /usr/include -type f -name '*.h' 2>/dev/null | head -4000 | xargs -r cat 2>/dev/null | head -c $((32 * 1024 * 1024)) > corpus-codec/code.txt
fi
if [ ! -d corpus-real ]; then
    mkdir -p corpus-real
    cp -r /usr/include corpus-real/include 2>/dev/null
    cp -r /usr/share/doc corpus-real/doc 2>/dev/null
    find /usr/lib -maxdepth 1 -name "*.so*" -size +100k -size -20M 2>/dev/null \
        | head -60 | xargs -I{} cp {} corpus-real/ 2>/dev/null
fi

REAL_BYTES=$(du -sb corpus-real | cut -f1)
REAL_FILES=$(find corpus-real -type f | wc -l)

echo "machine: $(lscpu | awk -F: '/Model name/{gsub(/^ +/,"",$2); print $2; exit}')"
echo "cores:   $(nproc)"
echo "corpora: real $((REAL_BYTES/1048576)) MiB / $REAL_FILES files"
echo "         text 1024 MiB (1 file, highly compressible)"
echo "         noise 512 MiB (1 file, incompressible)"
echo "         many  ~244 MiB / 5000 files (mixed)"
echo "         codec 64 MiB / 3 files (text, noise, code)"

hdr "1. CREATE — real corpus ($((REAL_BYTES/1048576)) MiB, $REAL_FILES files)"
rule
printf '%-26s %9s %14s %10s\n' TOOL "TIME(s)" "OUTPUT(B)" "vs zip -6"

rm -f r-*.zip
ZT=$(t zip -q -r r-zip.zip corpus-real); Z=$(sz r-zip.zip)
row() { printf '%-26s %9s %14s %+9.2f%%\n' "$1" "$2" "$3" "$(awk -v s="$3" -v z="$Z" 'BEGIN{print (s/z-1)*100}')"; }

x=$(t "$TTAR" create r-tt.zip corpus-real);               row "ttarchive (all cores)" "$x" "$(sz r-tt.zip)"
x=$(t "$TTAR" create r-t1.zip corpus-real --threads 1);   row "ttarchive (1 thread)"  "$x" "$(sz r-t1.zip)"
x=$(t "$TTAR" create r-tf.zip corpus-real --level fast);  row "ttarchive fast"        "$x" "$(sz r-tf.zip)"
x=$(t "$TTAR" create r-tb.zip corpus-real --level best);  row "ttarchive best"        "$x" "$(sz r-tb.zip)"
x=$(t "$TTAR" create r-ts.zip corpus-real --level store); row "ttarchive store"       "$x" "$(sz r-ts.zip)"
row "zip -6 (Info-ZIP)" "$ZT" "$Z"
have zip    && { x=$(t zip -q -1 -r r-z1.zip corpus-real); row "zip -1" "$x" "$(sz r-z1.zip)"; }
have zip    && { x=$(t zip -q -9 -r r-z9.zip corpus-real); row "zip -9" "$x" "$(sz r-z9.zip)"; }
have 7z     && { rm -f r-7z.zip; x=$(t 7z a -tzip -bso0 -bsp0 r-7z.zip corpus-real); row "7z -tzip" "$x" "$(sz r-7z.zip)"; }
have bsdtar && { x=$(t bsdtar -a -c -f r-bt.zip corpus-real); row "bsdtar (libarchive)" "$x" "$(sz r-bt.zip)"; }

hdr "2. EXTRACT — real corpus, from ttarchive's own archive"
rule
printf '%-26s %9s\n' TOOL "TIME(s)"
for n in 1 2 4 6 12; do
    rm -rf ox; x=$(t "$TTAR" extract r-tt.zip ox --skip-unsafe --threads $n)
    printf '%-26s %9s\n' "ttarchive ($n thread$([ $n -gt 1 ] && echo s))" "$x"
done
have unzip  && { rm -rf ox; x=$(t unzip -q -o r-tt.zip -d ox);       printf '%-26s %9s\n' "unzip (Info-ZIP)" "$x"; }
have 7z     && { rm -rf ox; x=$(t 7z x -y -bso0 -bsp0 r-tt.zip -oox); printf '%-26s %9s\n' "7z" "$x"; }
have bsdtar && { rm -rf ox; mkdir -p ox; x=$(t bsdtar -x -f r-tt.zip -C ox); printf '%-26s %9s\n' "bsdtar" "$x"; }
have python3 && { rm -rf ox; x=$(t python3 -c "import zipfile;zipfile.ZipFile('r-tt.zip').extractall('ox')"); printf '%-26s %9s\n' "python zipfile" "$x"; }
rm -rf ox

hdr "3. THREAD SCALING — create, real corpus"
rule
printf '%-26s %9s %9s\n' THREADS "TIME(s)" SPEEDUP
BASE=""
for n in 1 2 4 6 8 12; do
    rm -f s-$n.zip; x=$(t "$TTAR" create s-$n.zip corpus-real --threads $n)
    [ -z "$BASE" ] && BASE=$x
    printf '%-26s %9s %8sx\n' "$n" "$x" "$(awk -v b="$BASE" -v c="$x" 'BEGIN{printf "%.2f", b/c}')"
    rm -f s-$n.zip
done

hdr "4. DATA SHAPE — create (all cores unless noted)"
rule
printf '%-30s %9s %9s %9s %9s\n' CORPUS ttarchive "tt-1thr" "zip -6" "7z"
for c in text noise many; do
    rm -f d-*.zip
    a=$(t "$TTAR" create d-a.zip corpus-$c)
    b=$(t "$TTAR" create d-b.zip corpus-$c --threads 1)
    z="-"; have zip && z=$(t zip -q -r d-z.zip corpus-$c)
    s="-"; have 7z  && { rm -f d-7.zip; s=$(t 7z a -tzip -bso0 -bsp0 d-7.zip corpus-$c); }
    printf '%-30s %9s %9s %9s %9s\n' "$c" "$a" "$b" "$z" "$s"
    printf '%-30s %9s %9s %9s %9s\n' "  output MiB" \
        "$(( $(sz d-a.zip)/1048576 ))" "$(( $(sz d-b.zip)/1048576 ))" \
        "$(( $(sz d-z.zip)/1048576 ))" "$(( $(sz d-7.zip)/1048576 ))"
done

hdr "5. EXTRACT — by data shape, from ttarchive archives"
rule
printf '%-30s %9s %9s %9s %9s\n' CORPUS ttarchive "tt-1thr" unzip "7z"
for c in text noise many; do
    rm -f e-$c.zip; "$TTAR" create e-$c.zip corpus-$c >/dev/null 2>&1
    rm -rf ex; a=$(t "$TTAR" extract e-$c.zip ex)
    rm -rf ex; b=$(t "$TTAR" extract e-$c.zip ex --threads 1)
    rm -rf ex; u="-"; have unzip && u=$(t unzip -q -o e-$c.zip -d ex)
    rm -rf ex; s="-"; have 7z && s=$(t 7z x -y -bso0 -bsp0 e-$c.zip -oex)
    rm -rf ex; rm -f e-$c.zip
    printf '%-30s %9s %9s %9s %9s\n' "$c" "$a" "$b" "$u" "$s"
done

hdr "6. ENCRYPTION — 512 MiB incompressible (isolates cipher cost)"
rule
printf '%-30s %9s %9s\n' SCHEME "CREATE(s)" "EXTRACT(s)"
for scheme in aes256 aes192 aes128 zipcrypto; do
    rm -f k.zip
    c=$(t "$TTAR" create k.zip corpus-noise --password "$PW" --encryption $scheme)
    rm -rf kx; x=$(t "$TTAR" extract k.zip kx --password "$PW")
    printf '%-30s %9s %9s\n' "ttarchive $scheme" "$c" "$x"
    rm -rf kx
done
have 7z && {
    rm -f k7.zip; c=$(t 7z a -tzip -bso0 -bsp0 -p"$PW" -mem=AES256 -mm=Copy k7.zip corpus-noise)
    rm -rf kx; x=$(t 7z x -y -bso0 -bsp0 -p"$PW" k7.zip -okx)
    printf '%-30s %9s %9s\n' "7z AES256" "$c" "$x"; rm -rf kx k7.zip
}
have zip && {
    rm -f kz.zip; c=$(t zip -q -r -P "$PW" kz.zip corpus-noise)
    rm -rf kx; x="-"; have unzip && x=$(t unzip -q -o -P "$PW" kz.zip -d kx)
    printf '%-30s %9s %9s\n' "zip/unzip ZipCrypto" "$c" "$x"; rm -rf kx kz.zip
}
rm -f k.zip

hdr "7. MULTI-VOLUME — 512 MiB incompressible, 100 MiB volumes"
rule
printf '%-30s %9s %9s\n' TOOL "CREATE(s)" VOLUMES
rm -f v-*.z*
x=$(t "$TTAR" create v-tt.zip corpus-noise --volume-size 104857600)
printf '%-30s %9s %9s\n' "ttarchive" "$x" "$(ls v-tt.z* 2>/dev/null | wc -l)"
have zip && { x=$(t zip -q -r -s 100m v-zip.zip corpus-noise); printf '%-30s %9s %9s\n' "zip -s 100m" "$x" "$(ls v-zip.z* 2>/dev/null | wc -l)"; }
have 7z  && { x=$(t 7z a -tzip -bso0 -bsp0 -v100m v-7z.zip corpus-noise); printf '%-30s %9s %9s\n' "7z -v100m" "$x" "$(ls v-7z.zip.* 2>/dev/null | wc -l)"; }
rm -rf vx; x=$(t "$TTAR" extract v-tt.z01 vx); printf '%-30s %9s\n' "ttarchive extract from .z01" "$x"
rm -rf vx v-*.z*

hdr "8. CRYPTO PRIMITIVES"
rule
"$ROOT/target/release/examples/cryptobench" 2>/dev/null | sed 's/^/  /'

hdr "9. COMPRESSION METHODS — write path, codec corpus (64 MiB)"
rule
printf '%-30s %9s %9s %14s\n' METHOD "TIME(s)" "1-THREAD" "OUTPUT(B)"
for m in store deflate bzip2; do
    rm -f c-$m.zip c-$m-1.zip
    a=$(t "$TTAR" create c-$m.zip corpus-codec --method $m)
    b=$(t "$TTAR" create c-$m-1.zip corpus-codec --method $m --threads 1)
    printf '%-30s %9s %9s %14s\n' "ttarchive $m" "$a" "$b" "$(sz c-$m.zip)"
    rm -f c-$m-1.zip
done
have 7z && { rm -f c-7bz.zip; x=$(t 7z a -tzip -bso0 -bsp0 -mm=BZip2 c-7bz.zip corpus-codec); printf '%-30s %9s %9s %14s\n' "7z -mm=BZip2" "$x" "-" "$(sz c-7bz.zip)"; }
have zip && { rm -f c-zbz.zip; x=$(t zip -q -Z bzip2 -r c-zbz.zip corpus-codec); printf '%-30s %9s %9s %14s\n' "zip -Z bzip2" "$x" "-" "$(sz c-zbz.zip)"; }

hdr "10. COMPRESSION METHODS — read path, archives written by 7-Zip"
rule
printf '%-30s %9s %9s %14s\n' METHOD "ttarchive" "7z" "INPUT(B)"
have 7z && for m in Deflate Deflate64 BZip2 LZMA PPMd; do
    rm -f m-$m.zip
    7z a -tzip -bso0 -bsp0 -mm=$m m-$m.zip corpus-codec >/dev/null 2>&1 || { printf '%-30s %9s\n' "$m" "unsupported"; continue; }
    rm -rf mx; a=$(t "$TTAR" extract m-$m.zip mx)
    rm -rf mx; b=$(t 7z x -y -bso0 -bsp0 m-$m.zip -omx)
    printf '%-30s %9s %9s %14s\n' "$m" "$a" "$b" "$(sz m-$m.zip)"
    rm -rf mx m-$m.zip
done
have 7z || echo "  7-Zip not installed; nothing to read back"
rm -f c-*.zip

hdr "11. 7z — CREATE, real corpus ($((REAL_BYTES/1048576)) MiB, $REAL_FILES files)"
rule
printf '%-30s %9s %9s %14s\n' TOOL "TIME(s)" "PEAK(MiB)" "OUTPUT(B)"

rm -f r-*.7z
tm "$TTAR" create r-tt.7z corpus-real
printf '%-30s %9s %9s %14s\n' "ttarchive (all cores)" "$RUN_SECS" "$RUN_PEAK" "$(sz r-tt.7z)"
tm "$TTAR" create r-t1.7z corpus-real --threads 1
printf '%-30s %9s %9s %14s\n' "ttarchive (1 thread)" "$RUN_SECS" "$RUN_PEAK" "$(sz r-t1.7z)"
tm "$TTAR" create r-tf.7z corpus-real --level fast
printf '%-30s %9s %9s %14s\n' "ttarchive fast" "$RUN_SECS" "$RUN_PEAK" "$(sz r-tf.7z)"
tm "$TTAR" create r-tb.7z corpus-real --level best
printf '%-30s %9s %9s %14s\n' "ttarchive best" "$RUN_SECS" "$RUN_PEAK" "$(sz r-tb.7z)"
tm "$TTAR" create r-ts.7z corpus-real --level store
printf '%-30s %9s %9s %14s\n' "ttarchive store" "$RUN_SECS" "$RUN_PEAK" "$(sz r-ts.7z)"
for tool in 7z 7za; do
    have $tool && { rm -f r-$tool.7z; tm $tool a -bso0 -bsp0 r-$tool.7z corpus-real; printf '%-30s %9s %9s %14s\n' "$tool" "$RUN_SECS" "$RUN_PEAK" "$(sz r-$tool.7z)"; }
done
have bsdtar && { rm -f r-bt.7z; tm bsdtar -a -c -f r-bt.7z corpus-real; printf '%-30s %9s %9s %14s\n' "bsdtar (libarchive)" "$RUN_SECS" "$RUN_PEAK" "$(sz r-bt.7z)"; }

hdr "12. 7z — EXTRACT, from ttarchive's own archive"
rule
printf '%-30s %9s %9s\n' TOOL "TIME(s)" "PEAK(MiB)"
for n in 1 4 12; do
    rm -rf ox; tm "$TTAR" extract r-tt.7z ox --threads $n
    printf '%-30s %9s %9s\n' "ttarchive ($n thread$([ $n -gt 1 ] && echo s))" "$RUN_SECS" "$RUN_PEAK"
done
for tool in 7z 7za 7zr; do
    have $tool && { rm -rf ox; tm $tool x -y -bso0 -bsp0 r-tt.7z -oox; printf '%-30s %9s %9s\n' "$tool" "$RUN_SECS" "$RUN_PEAK"; }
done
have bsdtar && { rm -rf ox; mkdir -p ox; tm bsdtar -x -f r-tt.7z -C ox; printf '%-30s %9s %9s\n' "bsdtar" "$RUN_SECS" "$RUN_PEAK"; }
rm -rf ox

hdr "13. 7z CODERS — write path, codec corpus (64 MiB)"
rule
printf '%-30s %9s %9s %14s\n' CODER "TIME(s)" "PEAK(MiB)" "OUTPUT(B)"
rm -f q-*.7z
tm "$TTAR" create q-lzma2.7z corpus-codec
printf '%-30s %9s %9s %14s\n' "ttarchive LZMA2" "$RUN_SECS" "$RUN_PEAK" "$(sz q-lzma2.7z)"
for m in store bzip2 deflate; do
    tm "$TTAR" create q-$m.7z corpus-codec --method $m
    printf '%-30s %9s %9s %14s\n' "ttarchive $m" "$RUN_SECS" "$RUN_PEAK" "$(sz q-$m.7z)"
done
have 7z && for m in LZMA2 BZip2 Deflate Copy PPMd; do
    rm -f q-7-$m.7z
    tm 7z a -bso0 -bsp0 -m0=$m q-7-$m.7z corpus-codec
    printf '%-30s %9s %9s %14s\n' "7z -m0=$m" "$RUN_SECS" "$RUN_PEAK" "$(sz q-7-$m.7z)"
done

hdr "14. 7z CODERS AND FILTERS — read path, archives written by 7-Zip"
rule
printf '%-30s %9s %9s %9s %14s\n' CODER "ttarchive" "PEAK(MiB)" "7z" "INPUT(B)"
have 7z && for spec in "LZMA2:-m0=LZMA2" "LZMA:-m0=LZMA" "PPMd:-m0=PPMd" "BZip2:-m0=BZip2" "Deflate:-m0=Deflate" "Copy:-m0=Copy" "BCJ2:-m0=BCJ2" "BCJ:-m0=BCJ -m1=LZMA2" "ARM64:-m0=ARM64 -m1=LZMA2" "Delta:-m0=Delta:4 -m1=LZMA2"; do
    name="${spec%%:*}"; margs="${spec#*:}"
    rm -f n-$name.7z
    # shellcheck disable=SC2086
    7z a -bso0 -bsp0 $margs n-$name.7z corpus-codec >/dev/null 2>&1 || { printf '%-30s %9s\n' "$name" "unsupported"; continue; }
    rm -rf nx; tm "$TTAR" extract n-$name.7z nx
    a=$RUN_SECS; peak=$RUN_PEAK
    rm -rf nx; tm 7z x -y -bso0 -bsp0 n-$name.7z -onx
    printf '%-30s %9s %9s %9s %14s\n' "$name" "$a" "$peak" "$RUN_SECS" "$(sz n-$name.7z)"
    rm -rf nx n-$name.7z
done
have 7z || echo "  7-Zip not installed; nothing to read back"

hdr "15. 7z — ENCRYPTION, 512 MiB incompressible"
rule
printf '%-30s %9s %9s\n' MODE "CREATE(s)" "EXTRACT(s)"
rm -f p.7z
tm "$TTAR" create p.7z corpus-noise --password "$PW"; c=$RUN_SECS
rm -rf px; tm "$TTAR" extract p.7z px --password "$PW"
printf '%-30s %9s %9s\n' "ttarchive 7zAES + header" "$c" "$RUN_SECS"
rm -rf px
have 7z && {
    rm -f p7.7z; tm 7z a -bso0 -bsp0 -mhe=on -p"$PW" -m0=Copy p7.7z corpus-noise; c=$RUN_SECS
    rm -rf px; tm 7z x -y -bso0 -bsp0 -p"$PW" p7.7z -opx
    printf '%-30s %9s %9s\n' "7z -mhe=on -m0=Copy" "$c" "$RUN_SECS"; rm -rf px p7.7z
}
rm -f p.7z

hdr "16. 7z — VOLUMES, 512 MiB incompressible, 100 MiB volumes"
rule
printf '%-30s %9s %9s\n' TOOL "CREATE(s)" VOLUMES
rm -f w-*.7z*
tm "$TTAR" create w-tt.7z corpus-noise --volume-size 104857600
printf '%-30s %9s %9s\n' "ttarchive" "$RUN_SECS" "$(ls w-tt.7z.* 2>/dev/null | wc -l)"
have 7z && { tm 7z a -bso0 -bsp0 -v100m w-7z.7z corpus-noise; printf '%-30s %9s %9s\n' "7z -v100m" "$RUN_SECS" "$(ls w-7z.7z.* 2>/dev/null | wc -l)"; }
rm -rf wx; tm "$TTAR" extract w-tt.7z.001 wx
printf '%-30s %9s\n' "ttarchive from .001" "$RUN_SECS"
rm -rf wx w-*.7z*

hdr "17. 7z SOLID BLOCKS — pulling one file out of one folder"
rule
printf '%-30s %9s %9s %9s\n' BLOCK "TIME(s)" "PEAK(MiB)" "SIZE(B)"
have 7z && for block in 1m 16m 64m; do
    rm -f b-$block.7z
    7z a -bso0 -bsp0 -ms=on -ms=$block b-$block.7z corpus-codec >/dev/null 2>&1 || continue
    rm -rf bx; tm "$TTAR" extract b-$block.7z bx --select corpus-codec/noise.bin
    printf '%-30s %9s %9s %9s\n' "-ms=$block" "$RUN_SECS" "$RUN_PEAK" "$(sz b-$block.7z)"
    rm -rf bx b-$block.7z
done
rm -f q-*.7z r-*.7z

echo
echo "archives left in $WORK"
