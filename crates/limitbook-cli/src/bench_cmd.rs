//! The `bench` subcommand: whole-file throughput over a full ITCH capture.
//!
//! Reports four separately labeled rates, because they answer different
//! questions and must never be conflated:
//!
//! 1. **end-to-end** — gunzip + frame + decode + book apply, streaming from
//!    the `.gz` file. The full-pipeline rate.
//! 2. **full verification replay** — gunzip + the [`Replay`] engine (stats,
//!    invariant scoping, symbol cross-checks, final deep verify). This is
//!    what `limitbook replay --verify-every 0` runs; its violation count
//!    doubles as the correctness anchor for the whole day.
//! 3. **parse-only** — frame + decode over already-decompressed bytes held
//!    in memory (warm), CPU-bound, single-threaded.
//! 4. **parse + book** — frame + decode + book apply over the same
//!    in-memory bytes; the book is reconstructed from message 0.
//!
//! The decompressed file is cached next to the input (input path minus
//! `.gz`) so reruns skip decompression. `--flamegraph <out.svg>` (requires
//! building with `--features profiling`) profiles phase 4 with a sampling
//! signal-timer profiler and writes a flamegraph.

use std::fs::{self, File};
use std::hint::black_box;
use std::io::{BufReader, BufWriter, Read, Write};
use std::time::{Instant, UNIX_EPOCH};

use limitbook_core::book::Market;
use limitbook_core::frame::{MIN_MESSAGE_LEN, Messages};
use limitbook_core::parse;
use limitbook_core::replay::Replay;

use crate::{commas, open_capture, read_exact_or_end};

const USAGE: &str = "\
Usage: limitbook bench --input <capture.gz> [--only <phases>] [--flamegraph <out.svg>]

Measures whole-file throughput four ways, each labeled with what it
includes: end-to-end (gunzip + parse + book), full verification replay
(gunzip + Replay engine), parse-only (in-memory, warm), and parse + book
reconstruction (in-memory, warm). --only takes a comma list from
{e2e,verify,parse,book} (default: all). --flamegraph profiles the
parse+book phase and requires a build with --features profiling.
";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    E2e,
    Verify,
    Parse,
    Book,
}

pub fn bench(args: &[String]) -> Result<(), String> {
    let mut input = None;
    let mut flamegraph: Option<String> = None;
    let mut phases = vec![Phase::E2e, Phase::Verify, Phase::Parse, Phase::Book];
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let mut value = |name: &str| {
            it.next()
                .cloned()
                .ok_or_else(|| format!("{name} requires a value"))
        };
        match arg.as_str() {
            "--input" => input = Some(value("--input")?),
            "--flamegraph" => flamegraph = Some(value("--flamegraph")?),
            "--only" => {
                phases = value("--only")?
                    .split(',')
                    .map(|p| match p.trim() {
                        "e2e" => Ok(Phase::E2e),
                        "verify" => Ok(Phase::Verify),
                        "parse" => Ok(Phase::Parse),
                        "book" => Ok(Phase::Book),
                        other => Err(format!("unknown phase: {other}\n{USAGE}")),
                    })
                    .collect::<Result<_, _>>()?;
            }
            other => return Err(format!("unknown argument: {other}\n{USAGE}")),
        }
    }
    let input = input.ok_or_else(|| format!("--input is required\n{USAGE}"))?;
    if flamegraph.is_some() && !cfg!(feature = "profiling") {
        return Err("--flamegraph requires a build with --features profiling \
             (cargo run --release -p limitbook-cli --features profiling -- bench ...)"
            .to_string());
    }

    let gz_len = fs::metadata(&input)
        .map_err(|e| format!("stat {input}: {e}"))?
        .len();
    println!("bench: {input} ({} bytes)", commas(gz_len));

    if phases.contains(&Phase::E2e) {
        let (msgs, parse_errors, book_errors, live, secs) = stream_book(&input)?;
        report(
            "end-to-end: gunzip + frame + decode + book apply (streaming from .gz)",
            msgs,
            secs,
        );
        println!(
            "    parse errors {}, book errors {}, live orders at end {}",
            commas(parse_errors),
            commas(book_errors),
            commas(live as u64)
        );
    }

    if phases.contains(&Phase::Verify) {
        let (engine, secs) = stream_replay(&input)?;
        let stats = engine.stats();
        report(
            "full verification replay: gunzip + Replay engine (stats, invariant \
             scoping, final deep verify)",
            stats.messages,
            secs,
        );
        println!(
            "    violations {} -> {}",
            commas(stats.violations()),
            if engine.clean() { "CLEAN" } else { "FAILED" }
        );
    }

    if phases.contains(&Phase::Parse) || phases.contains(&Phase::Book) {
        let buf = decompressed_bytes(&input)?;

        if phases.contains(&Phase::Parse) {
            let start = Instant::now();
            let (msgs, ts_sum, parse_errors) = parse_pass(&buf)?;
            let secs = start.elapsed().as_secs_f64();
            report(
                "parse-only: frame + decode (decompressed bytes in RAM, warm, \
                 single thread)",
                msgs,
                secs,
            );
            println!(
                "    parse errors {}, timestamp checksum {ts_sum:#018x}",
                commas(parse_errors)
            );
        }

        if phases.contains(&Phase::Book) {
            let start = Instant::now();
            let (msgs, parse_errors, book_errors, live) =
                profiled(flamegraph.as_deref(), || book_pass(&buf))??;
            let secs = start.elapsed().as_secs_f64();
            report(
                "parse + book reconstruction: frame + decode + book apply \
                 (decompressed bytes in RAM, warm, single thread)",
                msgs,
                secs,
            );
            println!(
                "    parse errors {}, book errors {}, live orders at end {}",
                commas(parse_errors),
                commas(book_errors),
                commas(live as u64)
            );
        }
    }

    Ok(())
}

fn report(label: &str, msgs: u64, secs: f64) {
    let rate = msgs as f64 / secs;
    println!("\n[{label}]");
    println!(
        "    {} msgs in {secs:.1} s -> {} msg/s",
        commas(msgs),
        commas(rate as u64)
    );
}

/// Streams length-prefixed frames from a (possibly gzipped) capture into
/// `feed`. Returns the message count and elapsed seconds, timing the whole
/// pipeline including open, read, and gunzip.
fn stream_frames(input: &str, mut feed: impl FnMut(&[u8])) -> Result<(u64, f64), String> {
    let start = Instant::now();
    let mut reader = open_capture(input).map_err(|e| format!("open {input}: {e}"))?;
    let mut payload = vec![0u8; u16::MAX as usize];
    let mut msgs = 0u64;
    loop {
        let mut prefix = [0u8; 2];
        match read_exact_or_end(&mut reader, &mut prefix) {
            Ok(true) => {}
            Ok(false) => break,
            Err(e) => return Err(format!("read {input} after {msgs} messages: {e}")),
        }
        let len = u16::from_be_bytes(prefix) as usize;
        if len < MIN_MESSAGE_LEN {
            return Err(format!(
                "invalid frame after {msgs} messages: length {len} is below the \
                 {MIN_MESSAGE_LEN}-byte uniform header"
            ));
        }
        let body = &mut payload[..len];
        match read_exact_or_end(&mut reader, body) {
            Ok(true) => {}
            Ok(false) => return Err(format!("{input} ended mid-frame after {msgs} messages")),
            Err(e) => return Err(format!("read {input} after {msgs} messages: {e}")),
        }
        msgs += 1;
        feed(body);
    }
    Ok((msgs, start.elapsed().as_secs_f64()))
}

/// End-to-end phase: gunzip + frame + decode + `Market::apply`.
fn stream_book(input: &str) -> Result<(u64, u64, u64, usize, f64), String> {
    let mut market = Market::new();
    let mut parse_errors = 0u64;
    let mut book_errors = 0u64;
    let (msgs, secs) = stream_frames(input, |body| match parse::decode(body) {
        Ok(msg) => {
            if market.apply(&msg).is_err() {
                book_errors += 1;
            }
        }
        Err(_) => parse_errors += 1,
    })?;
    Ok((msgs, parse_errors, book_errors, market.live_orders(), secs))
}

/// Full verification phase: gunzip + the `Replay` engine, deep verify at
/// end of stream only (`verify_every = 0`).
fn stream_replay(input: &str) -> Result<(Replay, f64), String> {
    let mut engine = Replay::new(0);
    let (_, secs) = stream_frames(input, |body| engine.feed(body))?;
    engine.finish();
    Ok((engine, secs))
}

/// Returns the whole decompressed capture in memory, decompressing to a
/// cache file next to the input first (reused when it matches the `.gz`).
fn decompressed_bytes(input: &str) -> Result<Vec<u8>, String> {
    let raw_path = input
        .strip_suffix(".gz")
        .ok_or_else(|| format!("--input must be a .gz capture, got {input}"))?
        .to_string();
    let meta_path = format!("{raw_path}.limitbook-cache");
    let cache_key = gzip_cache_key(input)?;
    let cached = fs::metadata(&raw_path).map(|m| m.len()).unwrap_or(0);
    let valid_cache = cached != 0
        && fs::read_to_string(&meta_path)
            .map(|cached_key| cached_key == cache_key)
            .unwrap_or(false);
    if !valid_cache {
        let start = Instant::now();
        let gz = File::open(input).map_err(|e| format!("open {input}: {e}"))?;
        let mut decoder = flate2::bufread::GzDecoder::new(BufReader::with_capacity(1 << 20, gz));
        let tmp_path = format!("{raw_path}.{}.tmp", std::process::id());
        let out = File::create(&tmp_path).map_err(|e| format!("create {tmp_path}: {e}"))?;
        let mut writer = BufWriter::with_capacity(1 << 20, out);
        let bytes = std::io::copy(&mut decoder, &mut writer).map_err(|e| {
            let _ = fs::remove_file(&tmp_path);
            format!("decompress {input}: {e}")
        })?;
        writer
            .flush()
            .map_err(|e| format!("flush {tmp_path}: {e}"))?;
        drop(writer);
        fs::rename(&tmp_path, &raw_path)
            .map_err(|e| format!("replace {raw_path} with {tmp_path}: {e}"))?;
        fs::write(&meta_path, cache_key).map_err(|e| format!("write {meta_path}: {e}"))?;
        println!(
            "\ndecompressed {} bytes to {raw_path} in {:.1} s (cached for reruns)",
            commas(bytes),
            start.elapsed().as_secs_f64()
        );
    }
    let len = fs::metadata(&raw_path)
        .map_err(|e| format!("stat {raw_path}: {e}"))?
        .len() as usize;
    let start = Instant::now();
    let mut buf = vec![0u8; len];
    File::open(&raw_path)
        .and_then(|mut f| f.read_exact(&mut buf))
        .map_err(|e| format!("read {raw_path}: {e}"))?;
    println!(
        "\nloaded {} decompressed bytes into memory in {:.1} s",
        commas(len as u64),
        start.elapsed().as_secs_f64()
    );
    Ok(buf)
}

fn gzip_cache_key(input: &str) -> Result<String, String> {
    let meta = fs::metadata(input).map_err(|e| format!("stat {input}: {e}"))?;
    let mtime = meta
        .modified()
        .map_err(|e| format!("stat mtime {input}: {e}"))?
        .duration_since(UNIX_EPOCH)
        .map_err(|e| format!("stat mtime {input}: {e}"))?
        .as_nanos();
    Ok(format!("gz_len={}\ngz_mtime_ns={mtime}\n", meta.len()))
}

/// Parse-only pass over in-memory bytes. The wrapping timestamp sum keeps
/// every decode observably live (printed as a checksum).
fn parse_pass(buf: &[u8]) -> Result<(u64, u64, u64), String> {
    let mut msgs = 0u64;
    let mut ts_sum = 0u64;
    let mut parse_errors = 0u64;
    for payload in Messages::new(buf) {
        let payload = payload.map_err(|e| format!("framing error: {e:?}"))?;
        msgs += 1;
        match parse::decode(payload) {
            Ok(msg) => ts_sum = ts_sum.wrapping_add(msg.header().timestamp),
            Err(_) => parse_errors += 1,
        }
    }
    Ok((msgs, black_box(ts_sum), parse_errors))
}

/// Parse + book pass over in-memory bytes.
fn book_pass(buf: &[u8]) -> Result<(u64, u64, u64, usize), String> {
    let mut market = Market::new();
    let mut msgs = 0u64;
    let mut parse_errors = 0u64;
    let mut book_errors = 0u64;
    for payload in Messages::new(buf) {
        let payload = payload.map_err(|e| format!("framing error: {e:?}"))?;
        msgs += 1;
        match parse::decode(payload) {
            Ok(msg) => {
                if market.apply(&msg).is_err() {
                    book_errors += 1;
                }
            }
            Err(_) => parse_errors += 1,
        }
    }
    Ok((msgs, parse_errors, book_errors, market.live_orders()))
}

/// Runs `f` under the sampling profiler when a flamegraph path is given,
/// writing the SVG afterwards.
#[cfg(feature = "profiling")]
fn profiled<T>(flamegraph: Option<&str>, f: impl FnOnce() -> T) -> Result<T, String> {
    let Some(path) = flamegraph else {
        return Ok(f());
    };
    let guard = pprof::ProfilerGuardBuilder::default()
        .frequency(499)
        .blocklist(&["libc", "libgcc", "pthread", "vdso"])
        .build()
        .map_err(|e| format!("start profiler: {e}"))?;
    let result = f();
    let report = guard
        .report()
        .build()
        .map_err(|e| format!("build profile report: {e}"))?;
    let file = File::create(path).map_err(|e| format!("create {path}: {e}"))?;
    report
        .flamegraph(file)
        .map_err(|e| format!("write flamegraph {path}: {e}"))?;
    println!("\nflamegraph written to {path}");
    Ok(result)
}

#[cfg(not(feature = "profiling"))]
fn profiled<T>(flamegraph: Option<&str>, f: impl FnOnce() -> T) -> Result<T, String> {
    debug_assert!(flamegraph.is_none(), "checked at argument parsing");
    let _ = flamegraph;
    Ok(f())
}
