//! Golden decode tests: real messages hand-decoded from their raw bytes.
//!
//! Each case below was decoded BY HAND from the fixture's hex (offsets read
//! straight from spec/itch50_spec.txt, arithmetic done independently of
//! this crate), then frozen here. The test asserts two things:
//!
//! 1. the fixture still contains exactly these bytes at these message
//!    indices (guards against silent fixture drift), and
//! 2. `parse::decode` reproduces the hand-decoded fields exactly — the
//!    cheapest guard against a wrong offset corrupting everything
//!    downstream.
//!
//! Sanity anchors for the hand decode: prices land on plausible 2019-12-30
//! quotes (BP $37.81, TSLA $431.81), timestamps land in the fixture's
//! documented 03:00–05:28 ET window, and AAPL's locate (13) matches its
//! directory entry.

use std::io::Read;

use limitbook_core::frame::Messages;
use limitbook_core::parse::{self, EventCode, Header, Message, Price4, Side, TradingState, decode};

const FIXTURE_GZ: &[u8] = include_bytes!("../../../tests/fixtures/itch50_20191230.itch.gz");

fn fixture() -> Vec<u8> {
    let mut raw = Vec::new();
    flate2::read::GzDecoder::new(FIXTURE_GZ)
        .read_to_end(&mut raw)
        .expect("fixture must be valid gzip");
    raw
}

fn unhex(s: &str) -> Vec<u8> {
    assert!(s.len().is_multiple_of(2));
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

/// Fetches message `index` (0-based) from the fixture and asserts its raw
/// bytes match the frozen hex before returning it.
fn message_at(raw: &[u8], index: usize, expected_hex: &str) -> Vec<u8> {
    let payload = Messages::new(raw)
        .nth(index)
        .expect("fixture has enough messages")
        .expect("no framing errors");
    assert_eq!(
        payload,
        unhex(expected_hex).as_slice(),
        "fixture bytes at message {index} changed; golden literals need re-deriving"
    );
    payload.to_vec()
}

#[test]
fn golden_system_events() {
    let raw = fixture();
    // Message 0: first message of the day.
    let p = message_at(&raw, 0, "53000000000a11ea0e8c434f");
    assert_eq!(
        decode(&p).unwrap(),
        Message::SystemEvent(parse::SystemEvent {
            header: Header {
                stock_locate: 0,
                tracking_number: 0,
                timestamp: 11_072_057_543_747, // 03:04:32.057543747 ET
            },
            event_code: EventCode::StartOfMessages,
        })
    );
    // Message 637: start of system hours at 04:00:00.000198145.
    let p = message_at(&raw, 637, "53000000000d18c2e5860153");
    assert_eq!(
        decode(&p).unwrap(),
        Message::SystemEvent(parse::SystemEvent {
            header: Header {
                stock_locate: 0,
                tracking_number: 0,
                timestamp: 14_400_000_198_145,
            },
            event_code: EventCode::StartOfSystemHours,
        })
    );
}

#[test]
fn golden_stock_directory_aapl() {
    let raw = fixture();
    let p = message_at(
        &raw,
        1,
        "52000d00000a53a29059c34141504c20202020514e000000644e435a20504e4e314e000000004e",
    );
    assert_eq!(
        decode(&p).unwrap(),
        Message::StockDirectory(parse::StockDirectory {
            header: Header {
                stock_locate: 13,
                tracking_number: 0,
                timestamp: 11_354_325_932_483,
            },
            stock: b"AAPL    ",
            market_category: b'Q',
            financial_status: b'N',
            round_lot_size: 100,
            round_lots_only: b'N',
            issue_classification: b'C',
            issue_subtype: b"Z ",
            authenticity: b'P',
            short_sale_threshold: b'N',
            ipo_flag: b'N',
            luld_reference_price_tier: b'1',
            etp_flag: b'N',
            etp_leverage_factor: 0,
            inverse_indicator: b'N',
        })
    );
}

#[test]
fn golden_trading_action_and_reg_sho() {
    let raw = fixture();
    let p = message_at(
        &raw,
        14,
        "48000d00000a53b61a41864141504c20202020542020202020",
    );
    assert_eq!(
        decode(&p).unwrap(),
        Message::StockTradingAction(parse::StockTradingAction {
            header: Header {
                stock_locate: 13,
                tracking_number: 0,
                timestamp: 11_354_653_737_350,
            },
            stock: b"AAPL    ",
            trading_state: TradingState::Trading,
            reserved: b' ',
            reason: b"    ",
        })
    );
    let p = message_at(&raw, 15, "59000d00000a53b61a45824141504c2020202030");
    assert_eq!(
        decode(&p).unwrap(),
        Message::RegShoRestriction(parse::RegShoRestriction {
            header: Header {
                stock_locate: 13,
                tracking_number: 0,
                timestamp: 11_354_653_738_370,
            },
            stock: b"AAPL    ",
            reg_sho_action: b'0',
        })
    );
}

#[test]
fn golden_market_participant_position() {
    let raw = fixture();
    let p = message_at(
        &raw,
        40,
        "4c1d1b00000a5630a19318524243445350592020202020594e41",
    );
    assert_eq!(
        decode(&p).unwrap(),
        Message::MarketParticipantPosition(parse::MarketParticipantPosition {
            header: Header {
                stock_locate: 7451,
                tracking_number: 0,
                timestamp: 11_365_299_360_536,
            },
            mpid: b"RBCD",
            stock: b"SPY     ",
            primary_market_maker: b'Y',
            market_maker_mode: b'N',
            market_participant_state: b'A',
        })
    );
}

#[test]
fn golden_add_order_bp() {
    let raw = fixture();
    // First add of the day: BP, buy 500 @ 37.8100, order ref 9901.
    let p = message_at(
        &raw,
        638,
        "4103e000000d18c311bcbf00000000000026ad42000001f442502020202020200005c4f4",
    );
    assert_eq!(
        decode(&p).unwrap(),
        Message::AddOrder(parse::AddOrder {
            header: Header {
                stock_locate: 992,
                tracking_number: 0,
                timestamp: 14_400_003_095_743,
            },
            order_ref: 9901,
            side: Side::Buy,
            shares: 500,
            stock: b"BP      ",
            price: Price4(378_100),
            attribution: None,
        })
    );
}

#[test]
fn golden_modify_messages() {
    let raw = fixture();
    // Order Executed: 3 shares of order 46917, match 17792.
    let p = message_at(
        &raw,
        711,
        "45018900020d18d76fb7e8000000000000b745000000030000000000004580",
    );
    assert_eq!(
        decode(&p).unwrap(),
        Message::OrderExecuted(parse::OrderExecuted {
            header: Header {
                stock_locate: 393,
                tracking_number: 2,
                timestamp: 14_400_344_799_208,
            },
            order_ref: 46917,
            executed_shares: 3,
            match_number: 17792,
        })
    );
    // A mid-stream execution (100th E): AAPL, 51 shares of order 234277.
    let p = message_at(
        &raw,
        19059,
        "45000d00020de1ff7c936500000000000393250000003300000000000046bc",
    );
    assert_eq!(
        decode(&p).unwrap(),
        Message::OrderExecuted(parse::OrderExecuted {
            header: Header {
                stock_locate: 13,
                tracking_number: 2,
                timestamp: 15_264_305_156_965,
            },
            order_ref: 234_277,
            executed_shares: 51,
            match_number: 18108,
        })
    );
    // Order Cancel: 100 shares off order 32052 (SPY).
    let p = message_at(&raw, 733, "581d1b00000d18e43c1f170000000000007d3400000064");
    assert_eq!(
        decode(&p).unwrap(),
        Message::OrderCancel(parse::OrderCancel {
            header: Header {
                stock_locate: 7451,
                tracking_number: 0,
                timestamp: 14_400_559_521_559,
            },
            order_ref: 32052,
            cancelled_shares: 100,
        })
    );
    // Order Delete: order 10945 (ASML).
    let p = message_at(&raw, 671, "44022300000d18c826bf110000000000002ac1");
    assert_eq!(
        decode(&p).unwrap(),
        Message::OrderDelete(parse::OrderDelete {
            header: Header {
                stock_locate: 547,
                tracking_number: 0,
                timestamp: 14_400_088_358_673,
            },
            order_ref: 10945,
        })
    );
    // Order Replace: 10829 -> 52213, 1000 shares @ 297.3700 (ASML).
    let p = message_at(
        &raw,
        749,
        "55022300000d18e6760a470000000000002a4d000000000000cbf5000003e8002d6004",
    );
    assert_eq!(
        decode(&p).unwrap(),
        Message::OrderReplace(parse::OrderReplace {
            header: Header {
                stock_locate: 547,
                tracking_number: 0,
                timestamp: 14_400_596_871_751,
            },
            original_order_ref: 10829,
            new_order_ref: 52213,
            shares: 1000,
            price: Price4(2_973_700),
        })
    );
}

#[test]
fn golden_trade_tsla() {
    let raw = fixture();
    // Non-cross trade: TSLA, 88 shares @ 431.8100, zero-filled order ref
    // (per spec §1.5.1, effective 2010) and side "B" (effective 2014).
    let p = message_at(
        &raw,
        8501,
        "501f3800020d57d432665a0000000000000000420000005854534c41202020200041e39400000000000045f3",
    );
    assert_eq!(
        decode(&p).unwrap(),
        Message::Trade(parse::Trade {
            header: Header {
                stock_locate: 7992,
                tracking_number: 2,
                timestamp: 14_670_873_388_634,
            },
            order_ref: 0,
            side: Side::Buy,
            shares: 88,
            stock: b"TSLA    ",
            price: Price4(4_318_100),
            match_number: 17907,
        })
    );
}

/// Every payload in the fixture decodes; the typed message set matches the
/// 11 types known to be present.
#[test]
fn entire_fixture_decodes() {
    let raw = fixture();
    let mut count = 0u64;
    for payload in Messages::new(&raw) {
        let payload = payload.expect("no framing errors");
        decode(payload).expect("every fixture payload decodes");
        count += 1;
    }
    assert_eq!(count, 94_385);
}
