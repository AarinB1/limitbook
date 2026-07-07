//! CLI for the limitbook project. All file/OS I/O lives here;
//! `limitbook-core` stays pure computation.

use std::collections::{BTreeMap, HashSet};
use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::process::ExitCode;

use limitbook_core::frame::MIN_MESSAGE_LEN;

mod replay_cmd;

const USAGE: &str = "\
Usage: limitbook <command> [options]

Commands:
  replay        Stream a capture through parse + order book and report
                message counts, book activity, peak depth, and invariant
                results. Exits nonzero on any violation.
  make-fixture  Write a gzipped, symbol-filtered fixture from a capture.

Run a command without options for its usage. Captures may be raw or
gzipped, including truncated prefixes of a NASDAQ sample day.
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("make-fixture") => make_fixture(&args[1..]),
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

Reads a NASDAQ TotalView-ITCH 5.0 capture (raw or gzipped, may be a truncated
prefix of a sample day) and writes a gzipped, length-prefixed fixture
containing every System Event message plus the complete message stream for
the given stock symbols, selected by their Stock Locate codes as announced
in Stock Directory (R) messages.
";

fn make_fixture(args: &[String]) -> Result<(), String> {
    let mut input = None;
    let mut output = None;
    let mut symbols: HashSet<Vec<u8>> = HashSet::new();
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
            other => return Err(format!("unknown argument: {other}\n{MAKE_FIXTURE_USAGE}")),
        }
    }
    let input = input.ok_or_else(|| format!("--input is required\n{MAKE_FIXTURE_USAGE}"))?;
    let output = output.ok_or_else(|| format!("--output is required\n{MAKE_FIXTURE_USAGE}"))?;
    if symbols.is_empty() {
        return Err(format!("--symbols is required\n{MAKE_FIXTURE_USAGE}"));
    }

    let reader = open_capture(&input).map_err(|e| format!("open {input}: {e}"))?;
    let file = File::create(&output).map_err(|e| format!("create {output}: {e}"))?;
    let mut encoder =
        flate2::write::GzEncoder::new(BufWriter::new(file), flate2::Compression::best());

    let stats = filter_stream(reader, &symbols, &mut encoder)?;

    encoder
        .finish()
        .and_then(|w| w.into_inner().map_err(|e| e.into_error()))
        .and_then(|mut f| f.flush())
        .map_err(|e| format!("finish {output}: {e}"))?;

    eprintln!(
        "read {} messages, kept {} ({} symbols matched)",
        stats.read, stats.kept, stats.locates_matched
    );
    let mut types: Vec<_> = stats.kept_by_type.iter().collect();
    types.sort();
    for (ty, n) in types {
        eprintln!("  {} x {}", *ty as char, n);
    }
    Ok(())
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
