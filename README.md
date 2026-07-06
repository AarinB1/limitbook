# limitbook

Zero-copy NASDAQ ITCH 5.0 parser and limit order book reconstructor in Rust, with throughput benchmarking and queue-position tracking.

## Layout

- `crates/limitbook-core` — parser + book logic. Pure computation over byte slices (`#![no_std]`, no file/OS I/O), so it also compiles to `wasm32-unknown-unknown` for a browser demo. CI enforces this with a wasm build.
- `crates/limitbook-cli` — the `limitbook` binary; all file I/O and tooling lives here.
- `spec/itch50_spec.txt` — the official ITCH 5.0 specification as text; the source of truth for every message layout and field offset.
- `tests/fixtures/` — a ~1 MB real-data fixture (symbol-filtered slice of the public NASDAQ sample day 2019-12-30) so tests and CI never touch the multi-GB raw file. Regenerate with `tools/regen_fixture.sh`.

## Status

Phase 0 (scaffolding + ground truth) complete. Next: zero-copy parser, order book reconstruction, throughput benchmarks, WASM browser demo.
