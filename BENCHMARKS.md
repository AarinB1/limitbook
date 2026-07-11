# Benchmarks

Phase-3 measurement of the limitbook parser and order book over the full
NASDAQ TotalView-ITCH 5.0 sample day 2019-12-30: **268,744,780 messages**,
3,524,013,057 bytes gzipped, 8,251,407,909 bytes decompressed (~30.7
bytes/message including the 2-byte length prefix).

Every number below is labeled with exactly what it includes. The headline
rates come from pipelines that do different amounts of work (gunzip or
not; book reconstruction or not; invariant verification or not), and
conflating them would overstate any one of them.

## Hardware and conditions

- CPU: Intel Xeon @ 2.80 GHz (Cascade Lake, family 6 model 85 stepping 7),
  4 vCPUs on KVM, no SMT visible. Caches: 32 KiB L1d per core, 1 MiB L2
  per core, 33 MiB shared L3.
- RAM: 16 GiB, no swap.
- OS / toolchain: Linux 6.18, Rust 1.94.1, `--release` with thin LTO and
  `codegen-units = 1`.
- **Single core**: every run was pinned with `taskset -c 2`; the code under
  test is single-threaded throughout.
- **Warm, CPU-bound** for the in-memory numbers: the capture is fully
  decompressed into a heap buffer before timing starts, so those phases
  read RAM, not disk, and include no decompression work.
- Runner: `limitbook bench` (crates/limitbook-cli/src/bench_cmd.rs). Each
  phase prints its message count, error counters, and (for parse-only) a
  timestamp checksum, so every run re-verifies that the measured work
  actually happened. The checksum was identical across all runs.
- **Variance**: this is a shared cloud VM. Repeated runs of identical code
  moved individual numbers by roughly ±5–10%; treat the figures as that
  precise, no more. The relative story (parse vs. book vs. gunzip, and the
  before/after flamegraph shares) is the stable part.

## Whole-day throughput — the three labeled numbers

Final committed code, full 268,744,780-message day, single core:

| # | Pipeline | What it includes | Rate |
|---|----------|------------------|-----:|
| 1 | **End-to-end** | gunzip + frame + decode + book apply, streaming from the 3.5 GB `.gz` file | **≈1.15M msg/s** (234.6 s) |
| 2 | **Parse-only** | frame + decode, from decompressed bytes in RAM (warm, single core) | **≈31.9M msg/s** (8.4 s, ≈0.98 GB/s) |
| 3 | **Parse + book** | frame + decode + full book reconstruction from message 0, from decompressed bytes in RAM (warm, single core) | **≈1.38M msg/s** (195.3 s) |

Supplemental, same day and conditions:

| Pipeline | What it includes | Rate |
|----------|------------------|-----:|
| Full verification replay | gunzip + the `Replay` engine: stats, per-symbol tracking, crossed-book invariant scoping, symbol cross-checks, final deep consistency verify | ≈0.70M msg/s (385.3 s), **0 violations → CLEAN** |

Derived observations:

- **The book dominates, not the parser.** Parse-only runs ~23× faster than
  parse + book; ~94% of the parse+book pass is book maintenance
  (confirmed by the flamegraph below).
- **Gunzip cost**: end-to-end vs. in-memory parse+book differs by ~40–70 s
  across runs, consistent with flate2 streaming the day at ~120–200 MB/s
  of decompressed output on this core.
- End-of-day state is exactly as expected: 0 live orders after the close
  (the feed deletes everything), 0 parse errors, 0 book errors.

The pre-phase-3 "~480k msg/s end-to-end" figure was measured on different
hardware and is not directly comparable; on this VM the equivalent
pipelines measure 0.70M msg/s (with the full verification engine) and
1.15M msg/s (book reconstruction only). Cross-machine comparisons of any
of these numbers are not meaningful — rerun `limitbook bench` locally.

## Flamegraph: where the time actually goes

Profiled with a sampling profiler (pprof, 499 Hz, signal-timer based —
works without perf_event access) over the **entire** in-memory parse+book
pass, before touching any code:

**Baseline** — [docs/flamegraph-parse-book-baseline.svg](docs/flamegraph-parse-book-baseline.svg)
(48,337 samples ≈ 200 s):

| Frame | Share of pass |
|-------|--------------:|
| `Market::apply` (total) | 92.9% |
| ├─ `Market::remove_order` | 25.3% |
| ├─ `Market::refresh_armed` | **23.0%** |
| ├─ `Market::insert_order` | 17.2% |
| └─ apply's own hash-map validation / dispatch (inlined) | ~26% |
| `parse::decode` | 5.6% |

The expected suspects — BTreeMap price-level traversal and cache misses on
the order map — are real (`remove_order` + `insert_order` + the inlined
order-map lookups account for most of the pass), but the single biggest
*surprise* was `refresh_armed`: the crossed-book-invariant re-arming
check ran three B-tree lookups (`trading_state`, `books` + best-bid/ask,
`armed`) after **every** book mutation, almost always for a locate that
was already armed.

## The one optimization taken (and why it's safe)

`Market::refresh_armed` now returns early when the locate is already in
the `armed` set (and, before that, when outside market hours — the
original cheap exit). Arming is monotonic between disarm events: only a
trading-state transition or an end-of-hours system event removes entries,
and `refresh_armed`'s only effect is a set insert. If the locate is
already armed, the function was a provable no-op, so skipping it cannot
change any state — this is a pure removal of redundant reads.

Verification, per the phase-3 correctness anchor:

- `limitbook replay` output over the fixture is **byte-identical** to the
  pre-change baseline (checked with `diff`).
- The full 268.7M-message day still replays **CLEAN** through the complete
  verification engine: 0 parse errors, 0 book errors, 0 crossed-book
  violations, 0 consistency failures.
- The parse-only timestamp checksum is unchanged, and all tests, clippy,
  and the wasm32 build pass.

Before/after (same day, same conditions; both sides subject to the ±5–10%
run variance):

| Pipeline | Before | After | Change |
|----------|-------:|------:|-------:|
| Parse + book (in RAM) | 1.26M msg/s | 1.38–1.54M msg/s (two runs) | ≈ +10–23% |
| End-to-end (from .gz) | 0.95M msg/s | 1.15–1.24M msg/s (two runs) | ≈ +21–31% |
| Full verification replay | 0.64M msg/s | 0.70M msg/s | ≈ +10% |

**After** — [docs/flamegraph-parse-book-after.svg](docs/flamegraph-parse-book-after.svg)
(45,653 samples ≈ 191 s): `refresh_armed` drops from 23.0% to **7.5%**;
the pass is now dominated by `remove_order` (31.2%) and `insert_order`
(20.2%) — i.e. by the intended work: order-map updates and BTreeMap
price-level maintenance. Those are data-structure-level costs; per the
phase-3 scope (measure and understand, don't rewrite), they are left as
the documented starting point for any future optimization phase.

## Micro-benchmarks (criterion)

`cargo bench -p limitbook-core`, pinned to one core, on the final code.
Criterion reports a 95% confidence interval; the midpoints are quoted.

### `decode` — one message, success path (`benches/parse.rs`)

Synthetic payloads at the spec's exact per-type length. All 23 spec
message types fall in **22–43 ns**; the order-flow types that make up
~99% of a trading day:

| Type | Message | Time |
|------|---------|-----:|
| A | Add Order | 36.5 ns |
| F | Add Order w/ MPID | 35.8 ns |
| E | Order Executed | 33.5 ns |
| C | Order Executed w/ Price | 35.3 ns |
| X | Order Cancel | 28.9 ns |
| D | Order Delete | 26.0 ns |
| U | Order Replace | 36.0 ns |
| P | Trade (non-cross) | 40.6 ns |

### `Market::apply` — one operation against a warm book (`benches/book.rs`)

Warm book: one symbol, 100 price levels per side, 10 orders per level
(2,000 live orders), one-cent ticks around a $150.00 mid. Each sample
clones the market in (untimed) setup and applies exactly one message.

| Operation | Time |
|-----------|-----:|
| add, new price level (BTreeMap insert) | 464 ns |
| add, join existing level | 564 ns |
| execute, partial (front of best ask) | 420 ns |
| execute, full → order removed | 461 ns |
| cancel, partial | 494 ns |
| delete, back of a mid-book queue | 480 ns |
| replace → new price (remove + insert) | 668 ns |

### Whole-fixture passes (`benches/fixture.rs`)

The 94,385-message, 13-symbol checked-in fixture, in-memory:

| Pass | Rate |
|------|-----:|
| parse-only | 29.7M msg/s |
| parse + book | 7.7M msg/s |

The fixture's parse+book rate is ~5.6× the full-day rate: 13 symbols'
books fit in cache, the full market's ~8,000 do not. This gap is the
cache-miss cost of the real working set, and is why the resume number is
the full-day one, not this one.

## Reproducing

```sh
# fetch the sample day (~3.5 GB) to data/, then:
cargo build --release -p limitbook-cli
taskset -c 2 target/release/limitbook bench --input data/12302019.NASDAQ_ITCH50.gz

# flamegraph of the parse+book pass:
cargo build --release -p limitbook-cli --features profiling
taskset -c 2 target/release/limitbook bench --input data/12302019.NASDAQ_ITCH50.gz \
    --only book --flamegraph flamegraph.svg

# micro-benchmarks:
taskset -c 2 cargo bench -p limitbook-core
```
