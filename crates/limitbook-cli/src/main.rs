//! CLI for the limitbook project. All file/OS I/O lives here;
//! `limitbook-core` stays pure computation.

use std::collections::{BTreeMap, HashSet};
use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::process::ExitCode;

use limitbook_core::frame::MIN_MESSAGE_LEN;
use limitbook_core::parse::{Message, decode, trim_padding};

mod bench_cmd;
mod replay_cmd;

const USAGE: &str = "\
Usage: limitbook <command> [options]

Commands:
  replay        Stream a capture through parse + order book and report
                message counts, book activity, peak depth, and invariant
                results. Exits nonzero on any violation.
  bench         Measure whole-file throughput (end-to-end, parse-only,
                parse + book), each number labeled with what it includes.
  make-fixture  Write a gzipped, symbol-filtered fixture from a capture.

Run a command without options for its usage. Captures may be raw or
gzipped, including truncated prefixes of a NASDAQ sample day.
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("make-fixture") => make_fixture(&args[1..]),
        Some("bench") => bench_cmd::bench(&args[1..]),
        Some("replay") => {
            return match replay_cmd::replay(&args[1..]) {
                Ok(true) => ExitCode::SUCCESS,
                Ok(false) => ExitCode::FAILURE, // report printed; violations found
                Err(e) => {
                    eprintln!("error: {e}");
                    ExitCode::FAILURE
                }
            };
        }
        _ => {
            eprint!("{USAGE}");
            return ExitCode::FAILURE;
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

const MAKE_FIXTURE_USAGE: &str = "\
Usage: limitbook make-fixture --input <capture(.gz)> --output <fixture.itch.gz> --symbols <A,B,...>
                              [--start HH:MM:SS --end HH:MM:SS]

Reads a NASDAQ TotalView-ITCH 5.0 capture (raw or gzipped, may be a truncated
prefix of a sample day) and writes a gzipped, length-prefixed fixture
containing every System Event message plus the complete message stream for
the given stock symbols, selected by their Stock Locate codes as announced
in Stock Directory (R) messages.

With --start/--end (Eastern wall clock, matching the feed's ns-since-midnight
timestamps), the fixture is additionally cut to a time window while staying
self-contained: administrative state (S/R/H/Y/L) is kept from the head of the
day so trading phase and directory are correct, order flow is kept only for
orders ADDED inside the window, and executes/cancels/deletes/replaces are
kept only when they reference an order the fixture already contains — zero
dangling references by construction. Reading stops at the window end.
";

fn make_fixture(args: &[String]) -> Result<(), String> {
    let mut input = None;
    let mut output = None;
    let mut symbols: HashSet<Vec<u8>> = HashSet::new();
    let mut start = None;
    let mut end = None;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let mut value = |name: &str| {
            it.next()
                .cloned()
                .ok_or_else(|| format!("{name} requires a value"))
        };
        match arg.as_str() {
            "--input" => input = Some(value("--input")?),
            "--output" => output = Some(value("--output")?),
            "--symbols" => {
                for s in value("--symbols")?.split(',') {
                    let s = s.trim().to_ascii_uppercase();
                    if !s.is_empty() {
                        symbols.insert(s.into_bytes());
                    }
                }
            }
            "--start" => start = Some(parse_wall_clock(&value("--start")?)?),
            "--end" => end = Some(parse_wall_clock(&value("--end")?)?),
            other => return Err(format!("unknown argument: {other}\n{MAKE_FIXTURE_USAGE}")),
        }
    }
    let input = input.ok_or_else(|| format!("--input is required\n{MAKE_FIXTURE_USAGE}"))?;
    let output = output.ok_or_else(|| format!("--output is required\n{MAKE_FIXTURE_USAGE}"))?;
    if symbols.is_empty() {
        return Err(format!("--symbols is required\n{MAKE_FIXTURE_USAGE}"));
    }
    let window = match (start, end) {
        (Some(s), Some(e)) if s < e => Some((s, e)),
        (Some(_), Some(_)) => return Err("--start must be before --end".into()),
        (None, None) => None,
        _ => {
            return Err(format!(
                "--start and --end go together\n{MAKE_FIXTURE_USAGE}"
            ));
        }
    };

    let reader = open_capture(&input).map_err(|e| format!("open {input}: {e}"))?;
    let file = File::create(&output).map_err(|e| format!("create {output}: {e}"))?;
    let mut encoder =
        flate2::write::GzEncoder::new(BufWriter::new(file), flate2::Compression::best());

    let stats = match window {
        Some((start_ns, end_ns)) => {
            filter_stream_windowed(reader, &symbols, start_ns, end_ns, &mut encoder)?
        }
        None => filter_stream(reader, &symbols, &mut encoder)?,
    };

    encoder
        .finish()
        .and_then(|w| w.into_inner().map_err(|e| e.into_error()))
        .and_then(|mut f| f.flush())
        .map_err(|e| format!("finish {output}: {e}"))?;

    eprintln!(
        "read {} messages, kept {} ({} symbols matched), last timestamp {}",
        stats.read,
        stats.kept,
        stats.locates_matched,
        wall_clock(stats.last_ts)
    );
    let mut types: Vec<_> = stats.kept_by_type.iter().collect();
    types.sort();
    for (ty, n) in types {
        eprintln!("  {} x {}", *ty as char, n);
    }
    Ok(())
}

/// "HH:MM:SS" (feed wall clock, Eastern) to nanoseconds since midnight.
fn parse_wall_clock(s: &str) -> Result<u64, String> {
    let parts: Vec<&str> = s.split(':').collect();
    let [h, m, sec] = parts[..] else {
        return Err(format!("expected HH:MM:SS, got {s:?}"));
    };
    let field = |v: &str, max: u64, name: &str| -> Result<u64, String> {
        let n: u64 = v.parse().map_err(|e| format!("{name} in {s:?}: {e}"))?;
        if n > max {
            return Err(format!("{name} out of range in {s:?}"));
        }
        Ok(n)
    };
    let (h, m, sec) = (
        field(h, 23, "hours")?,
        field(m, 59, "minutes")?,
        field(sec, 59, "seconds")?,
    );
    Ok((h * 3600 + m * 60 + sec) * 1_000_000_000)
}

/// Nanoseconds since midnight as HH:MM:SS (whole seconds).
fn wall_clock(ns: u64) -> String {
    let secs = ns / 1_000_000_000;
    format!("{:02}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60)
}

/// Groups digits by thousands: 94385 -> "94,385".
pub(crate) fn commas(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Opens a capture file, transparently gunzipping if it has the gzip magic.
pub(crate) fn open_capture(path: &str) -> io::Result<Box<dyn Read>> {
    let mut file = File::open(path)?;
    let mut magic = [0u8; 2];
    let n = file.read(&mut magic)?;
    let file = File::open(path)?; // reopen to rewind
    let reader = BufReader::with_capacity(1 << 20, file);
    if n == 2 && magic == [0x1f, 0x8b] {
        Ok(Box::new(BufReader::with_capacity(
            1 << 20,
            flate2::bufread::GzDecoder::new(reader),
        )))
    } else {
        Ok(Box::new(reader))
    }
}

#[derive(Default)]
struct FilterStats {
    read: u64,
    kept: u64,
    locates_matched: usize,
    kept_by_type: BTreeMap<u8, u64>,
    /// Timestamp of the last message read (ns since midnight); tells the
    /// operator how deep into the day a truncated capture reaches.
    last_ts: u64,
}

/// Streams length-prefixed messages from `reader`, keeping System Event (S)
/// messages and every message whose Stock Locate belongs to one of
/// `symbols`. Locates are learned from Stock Directory (R) messages, which
/// precede all order flow in a sample day.
///
/// Field positions (committed spec, spec/itch50_spec.txt): all messages
/// carry Message Type at offset 0 and Stock Locate at offset 1 (2 bytes,
/// big-endian); the R message carries the 8-byte space-padded Stock symbol
/// at offset 11.
fn filter_stream(
    mut reader: Box<dyn Read>,
    symbols: &HashSet<Vec<u8>>,
    out: &mut impl Write,
) -> Result<FilterStats, String> {
    let mut stats = FilterStats::default();
    let mut wanted_locates: HashSet<u16> = HashSet::new();
    let mut payload = vec![0u8; u16::MAX as usize];
    loop {
        let mut prefix = [0u8; 2];
        match read_exact_or_end(&mut reader, &mut prefix) {
            Ok(true) => {}
            Ok(false) => break, // clean end of stream
            Err(e) => {
                truncation_warning(&e, stats.read)?;
                break;
            }
        }
        let len = u16::from_be_bytes(prefix) as usize;
        if len < MIN_MESSAGE_LEN {
            return Err(format!(
                "invalid frame after {} messages: length {len} is below the \
                 {MIN_MESSAGE_LEN}-byte uniform header",
                stats.read
            ));
        }
        let body = &mut payload[..len];
        match read_exact_or_end(&mut reader, body) {
            Ok(true) => {}
            Ok(false) => {
                truncation_warning(&io::Error::from(io::ErrorKind::UnexpectedEof), stats.read)?;
                break;
            }
            Err(e) => {
                truncation_warning(&e, stats.read)?;
                break;
            }
        }
        stats.read += 1;
        stats.last_ts = header_timestamp(body);

        let ty = body[0];
        let locate = u16::from_be_bytes([body[1], body[2]]);
        if ty == b'R' {
            let Some(stock_field) = body.get(11..19) else {
                return Err(format!(
                    "invalid Stock Directory (R) message at message {}: length {len} is too \
                     short for the Stock field (offset 11, 8 bytes)",
                    stats.read
                ));
            };
            let symbol = trim_trailing_spaces(stock_field);
            if symbols.contains(symbol) {
                wanted_locates.insert(locate);
            }
        }
        let keep = ty == b'S' || wanted_locates.contains(&locate);
        if keep {
            out.write_all(&prefix)
                .and_then(|()| out.write_all(body))
                .map_err(|e| format!("write output: {e}"))?;
            stats.kept += 1;
            *stats.kept_by_type.entry(ty).or_default() += 1;
        }
    }
    stats.locates_matched = wanted_locates.len();
    Ok(stats)
}

/// Timestamp from the uniform header: offset 5, 6 bytes big-endian, ns
/// since midnight (spec/itch50_spec.txt; `body` is at least
/// `MIN_MESSAGE_LEN` long).
fn header_timestamp(body: &[u8]) -> u64 {
    let mut ts = [0u8; 8];
    ts[2..8].copy_from_slice(&body[5..11]);
    u64::from_be_bytes(ts)
}

/// Time-windowed variant of [`filter_stream`]: cuts `[start_ns, end_ns)` out
/// of the middle of a day while keeping the fixture self-contained.
///
/// A literal time cut would strand order-flow messages whose Add happened
/// before the window, so this decodes every message (via the same
/// `limitbook-core` parser the replay engine uses) and filters by order
/// lifecycle instead:
///
/// - System Events plus per-symbol administrative state (R/H/Y/L) are kept
///   from the head of the day, so the directory, trading state, and market
///   phase are exactly what the full-day replay would have seen.
/// - Adds (A/F) are kept only when they land inside the window; their order
///   references are remembered.
/// - E/C/X/D/U are kept only when they reference a remembered order, so
///   every mutation resolves to an Add earlier in the fixture. A replace
///   chains: its new reference becomes remembered.
/// - Trades (P/Q/B) never touch the book (spec §1.5); in-window ones are
///   kept for wanted symbols so the tape has prints.
///
/// The kept stream is a subset of each symbol's real book, in original
/// order, so replaying it can never cross a book the real market didn't
/// cross. Reading stops at the first message timestamped at or past
/// `end_ns` (sample-day timestamps are monotonic).
fn filter_stream_windowed(
    mut reader: Box<dyn Read>,
    symbols: &HashSet<Vec<u8>>,
    start_ns: u64,
    end_ns: u64,
    out: &mut impl Write,
) -> Result<FilterStats, String> {
    let mut stats = FilterStats::default();
    let mut wanted_locates: HashSet<u16> = HashSet::new();
    let mut kept_refs: HashSet<u64> = HashSet::new();
    let mut payload = vec![0u8; u16::MAX as usize];
    loop {
        let mut prefix = [0u8; 2];
        match read_exact_or_end(&mut reader, &mut prefix) {
            Ok(true) => {}
            Ok(false) => break, // clean end of stream
            Err(e) => {
                truncation_warning(&e, stats.read)?;
                break;
            }
        }
        let len = u16::from_be_bytes(prefix) as usize;
        if len < MIN_MESSAGE_LEN {
            return Err(format!(
                "invalid frame after {} messages: length {len} is below the \
                 {MIN_MESSAGE_LEN}-byte uniform header",
                stats.read
            ));
        }
        let body = &mut payload[..len];
        match read_exact_or_end(&mut reader, body) {
            Ok(true) => {}
            Ok(false) => {
                truncation_warning(&io::Error::from(io::ErrorKind::UnexpectedEof), stats.read)?;
                break;
            }
            Err(e) => {
                truncation_warning(&e, stats.read)?;
                break;
            }
        }
        stats.read += 1;

        let msg = decode(body).map_err(|e| {
            format!(
                "message {} (type {:?}) failed to decode: {e:?}",
                stats.read, body[0] as char
            )
        })?;
        let ts = msg.header().timestamp;
        stats.last_ts = ts;
        if ts >= end_ns {
            break;
        }
        let wanted = wanted_locates.contains(&msg.header().stock_locate);
        let in_window = ts >= start_ns;

        let keep = match &msg {
            Message::SystemEvent(_) => true,
            Message::StockDirectory(m) => {
                let w = symbols.contains(trim_padding(m.stock));
                if w {
                    wanted_locates.insert(m.header.stock_locate);
                }
                w
            }
            Message::StockTradingAction(_)
            | Message::RegShoRestriction(_)
            | Message::MarketParticipantPosition(_) => wanted,
            Message::AddOrder(m) => {
                let keep = wanted && in_window;
                if keep {
                    kept_refs.insert(m.order_ref);
                }
                keep
            }
            Message::OrderExecuted(m) => kept_refs.contains(&m.order_ref),
            Message::OrderExecutedWithPrice(m) => kept_refs.contains(&m.order_ref),
            Message::OrderCancel(m) => kept_refs.contains(&m.order_ref),
            Message::OrderDelete(m) => kept_refs.remove(&m.order_ref),
            Message::OrderReplace(m) => {
                let keep = kept_refs.remove(&m.original_order_ref);
                if keep {
                    kept_refs.insert(m.new_order_ref);
                }
                keep
            }
            _ => wanted && in_window,
        };
        if keep {
            out.write_all(&prefix)
                .and_then(|()| out.write_all(body))
                .map_err(|e| format!("write output: {e}"))?;
            stats.kept += 1;
            *stats.kept_by_type.entry(body[0]).or_default() += 1;
        }
    }
    stats.locates_matched = wanted_locates.len();
    Ok(stats)
}

/// Reads exactly `buf.len()` bytes. Returns `Ok(false)` on a clean EOF
/// before any byte was read, `Err` on EOF partway through.
pub(crate) fn read_exact_or_end(reader: &mut impl Read, buf: &mut [u8]) -> io::Result<bool> {
    let mut filled = 0;
    while filled < buf.len() {
        match reader.read(&mut buf[filled..]) {
            Ok(0) if filled == 0 => return Ok(false),
            Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
            Ok(n) => filled += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(true)
}

/// A truncated gzip prefix of a sample day ends mid-stream; that is expected
/// and the messages read so far are still usable. Anything else is fatal.
pub(crate) fn truncation_warning(e: &io::Error, messages_read: u64) -> Result<(), String> {
    if e.kind() == io::ErrorKind::UnexpectedEof && messages_read > 0 {
        eprintln!(
            "note: input ended mid-stream after {messages_read} messages \
             (truncated capture); keeping messages read so far"
        );
        Ok(())
    } else {
        Err(format!("read input after {messages_read} messages: {e}"))
    }
}

fn trim_trailing_spaces(bytes: &[u8]) -> &[u8] {
    let end = bytes.iter().rposition(|&b| b != b' ').map_or(0, |i| i + 1);
    &bytes[..end]
}
