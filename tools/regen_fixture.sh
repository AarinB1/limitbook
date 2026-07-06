#!/usr/bin/env bash
# Regenerates tests/fixtures/itch50_20191230.itch.gz from the public NASDAQ
# TotalView-ITCH 5.0 sample day 2019-12-30.
#
# Only the first 16 MiB of the 3.5 GB sample file are needed: gzip streams
# decompress from the front, and that prefix already contains ~1.4M messages
# (through ~05:28 ET pre-market). Pass a path to a local capture (full .gz,
# truncated .gz, or raw .itch) to skip the download.
set -euo pipefail
cd "$(dirname "$0")/.."

URL='https://emi.nasdaq.com/ITCH/Nasdaq%20ITCH/12302019.NASDAQ_ITCH50.gz'
PREFIX_BYTES=16777215 # inclusive range end: first 16 MiB
SRC="${1:-data/12302019.NASDAQ_ITCH50.head16m.gz}"

# Symbols were chosen from activity in the prefix: household names for the
# eventual demo (execution-rich even pre-market) plus three high-volume ADRs
# for order-flow depth. Changing this list changes the expected counts in
# crates/limitbook-core/tests/fixture_integrity.rs.
SYMBOLS='AAPL,MSFT,TSLA,AMZN,GOOGL,FB,NFLX,AMD,SPY,QQQ,ASML,NVS,BP'

if [[ ! -f "$SRC" ]]; then
    mkdir -p "$(dirname "$SRC")"
    echo "downloading first 16 MiB of sample day to $SRC ..." >&2
    curl -sSf -r "0-$PREFIX_BYTES" -o "$SRC" "$URL"
fi

cargo run --release -p limitbook-cli -- make-fixture \
    --input "$SRC" \
    --output tests/fixtures/itch50_20191230.itch.gz \
    --symbols "$SYMBOLS"
