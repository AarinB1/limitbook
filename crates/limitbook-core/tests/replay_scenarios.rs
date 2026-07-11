//! Replay-engine scenarios on synthetic byte streams: the reopening-cross
//! arming suppression must not hide a persistent crossing — any book still
//! crossed and un-armed when market hours end is reported by the backstop.

use limitbook_core::replay::{Replay, Violation, ViolationKind};

/// Minimal spec-order encoder (offsets per spec/itch50_spec.txt).
fn hdr(ty: u8, locate: u16) -> Vec<u8> {
    let mut v = vec![ty];
    v.extend_from_slice(&locate.to_be_bytes());
    v.extend_from_slice(&0u16.to_be_bytes()); // tracking
    v.extend_from_slice(&[0; 6]); // timestamp
    v
}

fn system_event(code: u8) -> Vec<u8> {
    let mut v = hdr(b'S', 0);
    v.push(code);
    v
}

fn trading_action(locate: u16, state: u8) -> Vec<u8> {
    let mut v = hdr(b'H', locate);
    v.extend_from_slice(b"SYNTH   ");
    v.push(state);
    v.push(b' ');
    v.extend_from_slice(b"    ");
    v
}

fn add(locate: u16, oref: u64, side: u8, shares: u32, price: u32) -> Vec<u8> {
    let mut v = hdr(b'A', locate);
    v.extend_from_slice(&oref.to_be_bytes());
    v.push(side);
    v.extend_from_slice(&shares.to_be_bytes());
    v.extend_from_slice(b"SYNTH   ");
    v.extend_from_slice(&price.to_be_bytes());
    v
}

fn delete(locate: u16, oref: u64) -> Vec<u8> {
    let mut v = hdr(b'D', locate);
    v.extend_from_slice(&oref.to_be_bytes());
    v
}

fn crossings(replay: &Replay) -> Vec<&Violation> {
    replay
        .violations()
        .iter()
        .filter(|v| matches!(v.kind, ViolationKind::CrossedInContinuous { .. }))
        .collect()
}

/// A reopen whose unwind completes: zero violations end to end.
#[test]
fn resolved_reopen_is_clean() {
    let mut r = Replay::new(1);
    for msg in [
        system_event(b'Q'),            // market hours
        trading_action(1, b'H'),       // halt
        add(1, 10, b'B', 100, 53_000), // crossing interest during halt
        add(1, 11, b'S', 100, 51_000),
        trading_action(1, b'T'), // released while crossed
        delete(1, 10),           // unwind completes: uncrossed
        system_event(b'M'),      // market hours end
    ] {
        r.feed(&msg);
    }
    r.finish();
    assert_eq!(r.stats().crossed_in_continuous, 0, "{:?}", r.violations());
    assert!(r.clean());
}

/// A book that never uncrosses between release and close: exactly one
/// backstop violation at the end-of-market-hours event.
#[test]
fn persistent_crossing_reported_at_close() {
    let mut r = Replay::new(1);
    let msgs = [
        system_event(b'Q'),
        trading_action(1, b'H'),
        add(1, 10, b'B', 100, 53_000),
        add(1, 11, b'S', 100, 51_000),
        trading_action(1, b'T'), // released while crossed…
        // …and it never resolves.
        system_event(b'M'),
    ];
    let close_index = (msgs.len() - 1) as u64;
    for msg in &msgs {
        r.feed(msg);
    }
    r.finish();
    let crossed = crossings(&r);
    assert_eq!(crossed.len(), 1, "{:?}", r.violations());
    assert_eq!(crossed[0].msg_index, close_index, "reported at the close");
    assert!(!r.clean());
}

/// Stream ending mid-market-hours (truncated capture): the backstop runs
/// in finish() instead.
#[test]
fn persistent_crossing_reported_at_stream_end() {
    let mut r = Replay::new(1);
    for msg in [
        system_event(b'Q'),
        trading_action(1, b'H'),
        add(1, 10, b'B', 100, 53_000),
        add(1, 11, b'S', 100, 51_000),
        trading_action(1, b'T'),
    ] {
        r.feed(&msg);
    }
    r.finish();
    assert_eq!(crossings(&r).len(), 1, "{:?}", r.violations());
}

/// An armed book that crosses mid-session is flagged immediately, not at
/// the close — the arming grace applies only to reopen unwinds.
#[test]
fn armed_crossing_flagged_immediately() {
    let mut r = Replay::new(1);
    let msgs = [
        system_event(b'Q'),
        trading_action(1, b'T'), // released, book empty: armed
        add(1, 10, b'B', 100, 50_000),
        add(1, 11, b'S', 100, 51_000), // healthy
        add(1, 12, b'B', 10, 52_000),  // crosses: real violation NOW
    ];
    let crossing_index = (msgs.len() - 1) as u64;
    for msg in &msgs {
        r.feed(msg);
    }
    r.finish();
    let crossed = crossings(&r);
    assert_eq!(crossed.len(), 1, "{:?}", r.violations());
    assert_eq!(crossed[0].msg_index, crossing_index);
}
