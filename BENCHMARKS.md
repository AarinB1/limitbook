# Benchmarks

Phase-3 measurement of the limitbook parser and order book over the full
NASDAQ TotalView-ITCH 5.0 sample day 2019-12-30: **268,744,780 messages**,
3,524,013,057 bytes gzipped, 8,251,407,909 bytes decompressed (~30.7
bytes/message including the 2-byte length prefix).

Every number below is labeled with exactly what it includes. The three
headline rates come from pipelines that do different amounts of work
(gunzip or not; book reconstruction or not), and conflating them would
overstate any one of them.

## Hardware and conditions

- CPU: Intel Xeon @ 2.80 GHz (Cascade Lake, family 6 model 85 stepping 7),
  4 vCPUs on KVM, no SMT visible. Caches: 32 KiB L1d / core, 1 MiB L2 /
  core, 33 MiB shared L3.
- RAM: 16 GiB, no swap.
- OS/toolchain: Linux 6.18, Rust 1.94.1, `--release` with thin LTO and
  `codegen-units = 1`.
- Single core: every measurement was pinned with `taskset -c 2`; the code
  under test is single-threaded throughout.
- Warm and CPU-bound for the in-memory numbers: the capture is fully
  decompressed into a heap buffer before timing starts, so phases 2 and 3
  read RAM, not disk, and include no decompression work.
- Runner: `limitbook bench` (crates/limitbook-cli/src/bench_cmd.rs), which
  prints per-phase message counts, error counters, and a decode checksum so
  every run re-verifies that the measured work actually happened.

Caveat: this is a cloud VM. Absolute numbers move a few percent run to
run and would differ on other hardware; the relative story (parse vs book
vs gunzip) is the stable part.

## Whole-day throughput: the three labeled numbers

| # | Pipeline | What it includes | Throughput |
|---|----------|------------------|-----------:|
| 1 | End-to-end | gunzip + frame + decode + book apply, streaming from the 3.5 GB `.gz` | **TODO msg/s** |
| 2 | Parse-only | frame + decode from decompressed bytes in RAM (warm, single core) | **TODO msg/s** |
| 3 | Parse + book | frame + decode + full book reconstruction from decompressed bytes in RAM (warm, single core) | **TODO msg/s** |

TODO: numbers, verification replay, before/after, derived observations.

## Flamegraph (baseline, before any optimization)

TODO

## Micro-benchmarks (criterion)

TODO

## Correctness anchor

TODO
