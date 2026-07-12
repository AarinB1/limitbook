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

## `itch50_20191230_midday.itch.gz`

The browser demo's slice, and a second parity fixture: a ~1.6 MB gzipped
cut of the **same sample day during continuous trading**, 12:00–12:20 ET,
symbol-filtered to AAPL, TSLA, and SPY. Where the pre-market fixture is
thin (~500 executions over 2.5 hours), this one carries 157,824 messages
with ~6,000 executions and ~1,400 trade prints in 20 minutes — a book that
visibly churns.

A literal time cut would strand executes/cancels/deletes whose Add happened
before 12:00, so `make-fixture --start/--end` filters by order lifecycle:
System Events and per-symbol administrative state (R/H/Y/L) are kept from
the head of the day (correct directory, trading state, and market phase),
order flow is kept only for orders *added* inside the window, and every
mutation must reference an order already in the fixture. The result is
self-contained — zero dangling references — and, being an in-order subset
of each symbol's real book, it replays with zero invariant violations
under the armed continuous-trading crossed-book check.

Ground truth (`limitbook replay`): 157,824 messages, 13 types, 0
violations, 1,411 orders live at end; final quotes AAPL 291.0300/291.0500,
SPY 321.7000/321.7100, TSLA 418.9400/419.0500. The wasm-boundary parity
test (`crates/limitbook-wasm/tests/fixture_via_engine.rs`) and the browser
demo's end-of-replay verdict both assert this exact state.

Regenerate with `tools/regen_midday_fixture.sh` (downloads the first 2 GiB
of the sample day — mid-day data sits deep in the gzip stream).
