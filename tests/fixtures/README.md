# Test fixtures

## `itch50_20191230.itch.gz`

A ~1 MB gzipped, length-prefixed ITCH 5.0 capture used as ground truth by
tests and CI, so nothing ever needs the multi-GB raw sample file.

It is a **symbol-filtered slice** of the public NASDAQ sample day
2019-12-30 (`12302019.NASDAQ_ITCH50.gz`): from the first ~1.4M messages of
the day (03:00–~05:28 ET), it keeps every System Event message plus the
*complete* message stream for 13 symbols (AAPL, MSFT, TSLA, AMZN, GOOGL,
FB, NFLX, AMD, SPY, QQQ, ASML, NVS, BP), selected by Stock Locate code.

Filtering by symbol rather than taking a literal prefix is what makes the
fixture useful: the first 100k messages of any ITCH day are purely
administrative (stock directory, trading actions) with zero order flow,
while an unfiltered prefix with real flow gzips to several MB. Because
every message for a chosen locate is kept, per-symbol referential
integrity holds — every Execute/Cancel/Delete/Replace references an order
added earlier in the fixture (verified: 0 dangling references).

Contents: 94,385 messages, 11 message types
(S, R, H, Y, L, A, D, E, P, U, X), ~500 executions.

Regenerate with `tools/regen_fixture.sh` (downloads only the first 16 MiB
of the sample day). Raw captures are gitignored; only this fixture is
committed.
