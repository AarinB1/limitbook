//! The `replay` subcommand: stream a capture through parse + book and
//! report per-type counts, book activity, peaks, and invariant results.
//!
//! All the actual computation lives in `limitbook_core::replay::Replay`;
//! this module is I/O and formatting only.

use limitbook_core::frame::MIN_MESSAGE_LEN;
use limitbook_core::parse::{EventCode, Price4, trim_padding};
use limitbook_core::replay::{Replay, SymbolStats, Violation};

use crate::{commas, open_capture, read_exact_or_end, truncation_warning};

const USAGE: &str = "\
Usage: limitbook replay --input <capture(.gz)> [--verify-every <N>]

Streams a NASDAQ TotalView-ITCH 5.0 capture (raw or gzipped, may be a
truncated prefix of a sample day) through the parser and order book, then
reports message counts, book activity, peak depth, and invariant results.
Runs a deep book consistency check every N messages (default 1000; 0 =
only at end of stream). Exits nonzero if any invariant violation, dangling
reference, or parse error occurred.
";

pub fn replay(args: &[String]) -> Result<bool, String> {
    let mut input = None;
    let mut verify_every: u64 = 1000;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let mut value = |name: &str| {
            it.next()
                .cloned()
                .ok_or_else(|| format!("{name} requires a value"))
        };
        match arg.as_str() {
            "--input" => input = Some(value("--input")?),
            "--verify-every" => {
                verify_every = value("--verify-every")?
                    .parse()
                    .map_err(|e| format!("--verify-every: {e}"))?;
            }
            other => return Err(format!("unknown argument: {other}\n{USAGE}")),
        }
    }
    let input = input.ok_or_else(|| format!("--input is required\n{USAGE}"))?;

    let mut reader = open_capture(&input).map_err(|e| format!("open {input}: {e}"))?;
    let mut engine = Replay::new(verify_every);
    let mut payload = vec![0u8; u16::MAX as usize];
    let mut framing_error = None;
    loop {
        let mut prefix = [0u8; 2];
        match read_exact_or_end(&mut reader, &mut prefix) {
            Ok(true) => {}
            Ok(false) => break, // clean end of stream
            Err(e) => {
                truncation_warning(&e, engine.stats().messages)?;
                break;
            }
        }
        let len = u16::from_be_bytes(prefix) as usize;
        if len < MIN_MESSAGE_LEN {
            let error = format!(
                "invalid frame after {} messages: length {len} is below the \
                 {MIN_MESSAGE_LEN}-byte uniform header",
                engine.stats().messages
            );
            if engine.stats().messages == 0 {
                return Err(error);
            }
            framing_error = Some(error);
            break;
        }
        let body = &mut payload[..len];
        match read_exact_or_end(&mut reader, body) {
            Ok(true) => {}
            Ok(false) => {
                truncation_warning(
                    &std::io::Error::from(std::io::ErrorKind::UnexpectedEof),
                    engine.stats().messages,
                )?;
                break;
            }
            Err(e) => {
                truncation_warning(&e, engine.stats().messages)?;
                break;
            }
        }
        engine.feed(body);
    }
    engine.finish();

    print_report(&input, &engine, verify_every);
    if let Some(error) = framing_error {
        return Err(error);
    }
    Ok(engine.clean())
}

/// Fixed-point Price (4) as a decimal string, integer math only.
fn price(p: Price4) -> String {
    let (whole, frac) = p.split();
    format!("{whole}.{frac:04}")
}

/// Nanoseconds since midnight as HH:MM:SS.nnnnnnnnn.
fn clock(ns: u64) -> String {
    let (secs, nanos) = (ns / 1_000_000_000, ns % 1_000_000_000);
    format!(
        "{:02}:{:02}:{:02}.{:09}",
        secs / 3600,
        secs / 60 % 60,
        secs % 60,
        nanos
    )
}

fn type_name(ty: u8) -> &'static str {
    match ty {
        b'S' => "system event",
        b'R' => "stock directory",
        b'H' => "trading action",
        b'Y' => "reg SHO",
        b'L' => "participant position",
        b'V' => "MWCB decline levels",
        b'W' => "MWCB status",
        b'K' => "IPO quoting period",
        b'J' => "LULD auction collar",
        b'h' => "operational halt",
        b'A' => "add order",
        b'F' => "add order (MPID)",
        b'E' => "order executed",
        b'C' => "order executed w/ price",
        b'X' => "order cancel",
        b'D' => "order delete",
        b'U' => "order replace",
        b'P' => "trade (non-cross)",
        b'Q' => "cross trade",
        b'B' => "broken trade",
        b'I' => "NOII",
        b'N' => "RPII",
        b'O' => "DLCR price discovery",
        _ => "unknown",
    }
}

fn event_name(code: EventCode) -> &'static str {
    match code {
        EventCode::StartOfMessages => "start of messages",
        EventCode::StartOfSystemHours => "start of system hours",
        EventCode::StartOfMarketHours => "start of market hours",
        EventCode::EndOfMarketHours => "end of market hours",
        EventCode::EndOfSystemHours => "end of system hours",
        EventCode::EndOfMessages => "end of messages",
    }
}

fn symbol_name(replay: &Replay, locate: u16) -> String {
    match replay.symbols().get(&locate) {
        Some(stock) => String::from_utf8_lossy(trim_padding(stock)).into_owned(),
        None => format!("locate {locate}"),
    }
}

fn print_report(input: &str, engine: &Replay, verify_every: u64) {
    let stats = engine.stats();
    let market = engine.market();

    println!("replay: {input}");
    let type_count = stats.by_type.iter().filter(|&&n| n > 0).count();
    println!(
        "messages: {} ({} message types)",
        commas(stats.messages),
        type_count
    );
    for (ty, &n) in stats.by_type.iter().enumerate() {
        if n > 0 {
            println!(
                "  {} {:<24} {:>10}",
                ty as u8 as char,
                type_name(ty as u8),
                commas(n)
            );
        }
    }

    println!("\nsystem events:");
    for &(code, ts) in engine.system_events() {
        println!("  {:<24} {}", event_name(code), clock(ts));
    }
    println!(
        "market phase at end: {}",
        if market.market_hours() {
            "market hours (continuous trading)"
        } else {
            "outside market hours (books may legitimately cross)"
        }
    );

    println!("\nbook activity:");
    println!(
        "  orders added (A+F)      {:>10}   {:>13} shares",
        commas(stats.orders_added),
        commas(stats.shares_added)
    );
    println!(
        "  executed (E)            {:>10}   {:>13} shares",
        commas(stats.exec_events),
        commas(stats.exec_shares)
    );
    println!(
        "  executed w/ price (C)   {:>10}   {:>13} shares ({} non-printable)",
        commas(stats.exec_with_price_events),
        commas(stats.exec_with_price_shares),
        commas(stats.exec_nonprintable)
    );
    println!(
        "  orders fully filled     {:>10}",
        commas(stats.orders_filled)
    );
    println!(
        "  partial cancels (X)     {:>10}   {:>13} shares ({} cancelled to zero)",
        commas(stats.cancel_events),
        commas(stats.cancelled_shares),
        commas(stats.orders_cancelled_out)
    );
    println!(
        "  deletes (D)             {:>10}   {:>13} shares removed",
        commas(stats.deletes),
        commas(stats.deleted_shares)
    );
    println!("  replaces (U)            {:>10}", commas(stats.replaces));
    println!(
        "  non-cross trades (P)    {:>10}   {:>13} shares",
        commas(stats.trades),
        commas(stats.trade_shares)
    );
    println!(
        "  cross trades (Q)        {:>10}   {:>13} shares",
        commas(stats.cross_trades),
        commas(stats.cross_shares)
    );
    println!(
        "  live at end             {:>10} orders in {} books",
        commas(market.live_orders() as u64),
        market.books().count()
    );

    println!("\npeak depth:");
    println!(
        "  live orders, all books  {:>10}   (at message {})",
        commas(stats.peak_live_orders as u64),
        commas(stats.peak_live_orders_at)
    );
    println!(
        "  live orders, one book   {:>10}   ({})",
        commas(stats.peak_book_orders as u64),
        symbol_name(engine, stats.peak_book_orders_locate)
    );
    println!(
        "  price levels, one side  {:>10}   ({})",
        commas(stats.peak_side_levels as u64),
        symbol_name(engine, stats.peak_side_levels_locate)
    );

    println!("\nintegrity:");
    println!(
        "  parse errors                     {:>8}",
        commas(stats.parse_errors)
    );
    println!(
        "  book errors (dangling refs etc.) {:>8}",
        commas(stats.book_errors)
    );
    println!(
        "  crossed in continuous trading    {:>8}",
        commas(stats.crossed_in_continuous)
    );
    println!(
        "  add-vs-directory symbol checks   {:>8} mismatches",
        commas(stats.symbol_mismatches)
    );
    println!(
        "  timestamp regressions            {:>8}",
        commas(stats.timestamp_regressions)
    );
    let cadence = if verify_every > 0 {
        format!("every {} messages + final", commas(verify_every))
    } else {
        "at end of stream".to_string()
    };
    println!(
        "  deep consistency check ({cadence}): {}",
        if stats.consistency_failures == 0 {
            "OK"
        } else {
            "FAILED"
        }
    );

    // Per-symbol table, top 20 by adds if the directory is large.
    let mut rows: Vec<(u16, &SymbolStats)> =
        engine.per_symbol().iter().map(|(l, s)| (*l, s)).collect();
    if rows.len() > 20 {
        rows.sort_by_key(|(_, s)| core::cmp::Reverse(s.adds));
        rows.truncate(20);
        rows.sort_by_key(|(l, _)| *l);
        println!(
            "\nper-symbol (top 20 of {} by adds):",
            engine.per_symbol().len()
        );
    } else {
        println!("\nper-symbol:");
    }
    println!(
        "  {:<8} {:>8} {:>6} {:>10} {:>8} {:>8} {:>8} {:>6}  {:>12} / {:<12}",
        "symbol",
        "adds",
        "execs",
        "exec_sh",
        "cancels",
        "deletes",
        "replaces",
        "live",
        "best bid",
        "best ask"
    );
    for (locate, sym) in rows {
        let book = market.book(locate);
        let (live, bid, ask) = match book {
            Some(b) => (
                b.order_count(),
                fmt_quote(b.best_bid()),
                fmt_quote(b.best_ask()),
            ),
            None => (0, "-".into(), "-".into()),
        };
        println!(
            "  {:<8} {:>8} {:>6} {:>10} {:>8} {:>8} {:>8} {:>6}  {:>12} / {:<12}",
            symbol_name(engine, locate),
            commas(sym.adds),
            commas(sym.exec_events),
            commas(sym.exec_shares),
            commas(sym.cancels),
            commas(sym.deletes),
            commas(sym.replaces),
            commas(live as u64),
            bid,
            ask
        );
    }

    let violations = stats.violations();
    if violations > 0 {
        println!(
            "\nviolations ({} total, first {} shown):",
            commas(violations),
            engine.violations().len()
        );
        for Violation { msg_index, kind } in engine.violations() {
            println!("  message {:>10}: {kind:?}", commas(*msg_index));
        }
        println!("\nRESULT: FAILED — {} violations", commas(violations));
    } else {
        println!("\nRESULT: CLEAN — zero invariant violations, zero dangling references");
    }
}

fn fmt_quote(q: Option<(Price4, u64)>) -> String {
    match q {
        Some((p, _)) => price(p),
        None => "-".into(),
    }
}
