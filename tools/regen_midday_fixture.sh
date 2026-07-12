#!/usr/bin/env bash
# Regenerates tests/fixtures/itch50_20191230_midday.itch.gz — the browser
# demo's slice — from the public NASDAQ TotalView-ITCH 5.0 sample day
# 2019-12-30.
#
# Unlike the pre-market fixture (tools/regen_fixture.sh), this slice sits in
# the middle of continuous trading, so the cut needs the first 2 GiB of the
# 3.5 GB sample file: gzip streams decompress from the front, and that
# prefix reaches ~12:26 ET (~145M messages). Pass a path to a local capture
# to skip the download.
#
# Changing the symbols or window changes the expected counts in
# crates/limitbook-wasm/tests/fixture_via_engine.rs and the constants in
# web/main.js.
set -euo pipefail
cd "$(dirname "$0")/.."

URL='https://emi.nasdaq.com/ITCH/Nasdaq%20ITCH/12302019.NASDAQ_ITCH50.gz'
PREFIX_BYTES=2147483647 # inclusive range end: first 2 GiB
SRC="${1:-data/12302019.NASDAQ_ITCH50.head2g.gz}"

# Three liquid household names, busy through the mid-day lull, so the demo
# book visibly churns at 1x. The 20-minute window keeps the fixture ~1.6 MB
# gzipped. --start/--end make the cut lifecycle-aware: only orders ADDED
# inside the window (plus everything referencing them) are kept, so the
# fixture is self-contained with zero dangling references.
SYMBOLS='AAPL,TSLA,SPY'
START='12:00:00'
END='12:20:00'

if [[ ! -f "$SRC" ]]; then
    mkdir -p "$(dirname "$SRC")"
    echo "downloading first 2 GiB of sample day to $SRC ..." >&2
    curl -sSf -r "0-$PREFIX_BYTES" -o "$SRC" "$URL"
fi

cargo run --release -p limitbook-cli -- make-fixture \
    --input "$SRC" \
    --output tests/fixtures/itch50_20191230_midday.itch.gz \
    --symbols "$SYMBOLS" \
    --start "$START" \
    --end "$END"
