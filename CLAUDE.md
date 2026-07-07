# limitbook

Zero-copy NASDAQ TotalView-ITCH 5.0 parser + limit order book in Rust.

## Layout

- `crates/limitbook-core` — parser + book. Pure computation over byte
  slices: `#![no_std]`, NO file/OS I/O, every dependency must build for
  `wasm32-unknown-unknown` (CI enforces with a wasm build). Dev-dependencies
  are exempt.
- `crates/limitbook-cli` — binary `limitbook`. All I/O lives here.
- `tests/fixtures/` — checked-in ground truth (~1 MB gzipped, symbol-filtered
  slice of sample day 2019-12-30). Regenerate: `tools/regen_fixture.sh`.
  Tests and CI must never require the multi-GB raw file; raw `.itch`/`.gz`
  captures are gitignored.

## Source of truth

`spec/itch50_spec.txt` (official spec, text-extracted) defines every message
layout and field offset. Take offsets from it, never from memory. All ITCH 5.0
messages share a uniform header: type (offset 0), stock locate (1, u16),
tracking number (3, u16), timestamp (5, 6 bytes, ns since midnight). All
integers big-endian; sample files frame each payload with a u16 length prefix.

## Book invariants

- Best bid < best ask during continuous trading.
- Executed shares never exceed the resting order's shares.
- Deletes, cancels, and replaces must reference a live order.

## Build phases

0. Scaffolding + ground truth (done) → 1. zero-copy parser (done) →
2. order book (done; `limitbook replay` replays the fixture with zero
invariant violations) → 3. throughput/benchmarks + queue-position tracking →
4. WASM browser demo.

## CI

Every push: `cargo fmt --check`, `cargo clippy --workspace --all-targets
-- -D warnings`, `cargo test --workspace`, core wasm32 build. Keep all four
green.
