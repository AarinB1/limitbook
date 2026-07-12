# limitbook

Zero-copy NASDAQ ITCH 5.0 parser and limit order book reconstructor in Rust, with throughput benchmarking and queue-position tracking.

**[Live demo](https://aarinb1.github.io/limitbook/)** — the engine compiled
to WebAssembly, replaying real Nasdaq depth (AAPL · TSLA · SPY, 2019-12-30
mid-day) in the browser. The replay's final state is checked tick-for-tick
against `limitbook replay`'s output for the same slice, in the page and in CI.

## Layout

- `crates/limitbook-core` — parser + book logic. Pure computation over byte slices (`#![no_std]`, no file/OS I/O), so it also compiles to `wasm32-unknown-unknown` for the browser demo. CI enforces this with a wasm build.
- `crates/limitbook-cli` — the `limitbook` binary; all file I/O and tooling lives here.
- `crates/limitbook-wasm` — thin wasm-bindgen glue exposing the core replay engine to `web/`; no market logic of its own.
- `web/` — the browser demo (static HTML/JS + the wasm module; `web/build.sh` builds it, `.github/workflows/pages.yml` deploys it).
- `spec/itch50_spec.txt` — the official ITCH 5.0 specification as text; the source of truth for every message layout and field offset.
- `tests/fixtures/` — two ~1 MB real-data fixtures (symbol-filtered slices of the public NASDAQ sample day 2019-12-30): a pre-market slice and the mid-day continuous-trading slice the demo replays. Tests and CI never touch the multi-GB raw file. Regenerate with `tools/regen_fixture.sh` / `tools/regen_midday_fixture.sh`.

## Status

Phases 0–4 complete: zero-copy parser, order book reconstruction (the full
268.7M-message sample day replays with zero invariant violations),
throughput benchmarks — see [BENCHMARKS.md](BENCHMARKS.md) for the
measured rates, methodology, and flamegraphs — and the WASM browser demo
above. Next: queue-position tracking.
