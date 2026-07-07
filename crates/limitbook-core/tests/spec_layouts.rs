//! Synthetic layout tests: every message type in the spec, constructed
//! byte-by-byte in field order per its table in spec/itch50_spec.txt, then
//! decoded and compared field-for-field.
//!
//! The builder appends fields in spec order, so offsets are implied by the
//! declared field sequence — an independent restatement of the layout that
//! would disagree with `parse::decode`'s explicit offsets if either were
//! wrong. Covers the types absent from the fixture (F, C, V, W, K, J, h,
//! Q, B, I, N, O) as well as the exact-length and invalid-code error paths.

use limitbook_core::parse::{
    self, EventCode, Header, Message, ParseError, Price4, Price8, Side, TradingState, decode,
    message_len,
};

/// Appends big-endian fields in spec table order.
struct Enc(Vec<u8>);

impl Enc {
    /// Starts a message: type byte + the uniform header
    /// (locate @1, tracking @3, timestamp @5 as 6 bytes).
    fn new(ty: u8, h: &Header) -> Enc {
        let mut e = Enc(vec![ty]);
        e.u16(h.stock_locate);
        e.u16(h.tracking_number);
        e.0.extend_from_slice(&h.timestamp.to_be_bytes()[2..8]);
        e
    }
    fn u8(&mut self, v: u8) -> &mut Self {
        self.0.push(v);
        self
    }
    fn u16(&mut self, v: u16) -> &mut Self {
        self.0.extend_from_slice(&v.to_be_bytes());
        self
    }
    fn u32(&mut self, v: u32) -> &mut Self {
        self.0.extend_from_slice(&v.to_be_bytes());
        self
    }
    fn u64(&mut self, v: u64) -> &mut Self {
        self.0.extend_from_slice(&v.to_be_bytes());
        self
    }
    fn bytes(&mut self, v: &[u8]) -> &mut Self {
        self.0.extend_from_slice(v);
        self
    }
    /// Asserts the built length matches the spec table total for the type.
    fn done(self) -> Vec<u8> {
        assert_eq!(
            Some(self.0.len()),
            message_len(self.0[0]),
            "built length disagrees with message_len({:?})",
            self.0[0] as char
        );
        self.0
    }
}

fn hdr() -> Header {
    Header {
        stock_locate: 0x0102,
        tracking_number: 0x0304,
        // Distinct bytes in all six timestamp positions.
        timestamp: 0x0000_a1b2_c3d4_e5f6,
    }
}

#[test]
fn system_event() {
    // §1.1: Event Code @11.
    let mut e = Enc::new(b'S', &hdr());
    e.u8(b'Q');
    assert_eq!(
        decode(&e.done()).unwrap(),
        Message::SystemEvent(parse::SystemEvent {
            header: hdr(),
            event_code: EventCode::StartOfMarketHours,
        })
    );
}

#[test]
fn stock_directory() {
    // §1.2.1 field order: Stock, Market Category, Financial Status, Round
    // Lot Size, Round Lots Only, Issue Classification, Issue Sub-Type,
    // Authenticity, Short Sale Threshold, IPO Flag, LULD Tier, ETP Flag,
    // ETP Leverage Factor, Inverse Indicator.
    let mut e = Enc::new(b'R', &hdr());
    e.bytes(b"QQQ     ")
        .u8(b'G')
        .u8(b'D')
        .u32(100)
        .u8(b'Y')
        .u8(b'Q')
        .bytes(b"EM")
        .u8(b'T')
        .u8(b'N')
        .u8(b'Y')
        .u8(b'2')
        .u8(b'Y')
        .u32(3)
        .u8(b'Y');
    assert_eq!(
        decode(&e.done()).unwrap(),
        Message::StockDirectory(parse::StockDirectory {
            header: hdr(),
            stock: b"QQQ     ",
            market_category: b'G',
            financial_status: b'D',
            round_lot_size: 100,
            round_lots_only: b'Y',
            issue_classification: b'Q',
            issue_subtype: b"EM",
            authenticity: b'T',
            short_sale_threshold: b'N',
            ipo_flag: b'Y',
            luld_reference_price_tier: b'2',
            etp_flag: b'Y',
            etp_leverage_factor: 3,
            inverse_indicator: b'Y',
        })
    );
}

#[test]
fn stock_trading_action_states() {
    // §1.2.2: Stock, Trading State, Reserved, Reason.
    for (byte, state) in [
        (b'H', TradingState::Halted),
        (b'P', TradingState::Paused),
        (b'Q', TradingState::QuotationOnly),
        (b'T', TradingState::Trading),
    ] {
        let mut e = Enc::new(b'H', &hdr());
        e.bytes(b"AAPL    ").u8(byte).u8(b' ').bytes(b"IPO1");
        assert_eq!(
            decode(&e.done()).unwrap(),
            Message::StockTradingAction(parse::StockTradingAction {
                header: hdr(),
                stock: b"AAPL    ",
                trading_state: state,
                reserved: b' ',
                reason: b"IPO1",
            })
        );
    }
}

#[test]
fn reg_sho() {
    // §1.2.3: Stock, Reg SHO Action.
    let mut e = Enc::new(b'Y', &hdr());
    e.bytes(b"AMD     ").u8(b'1');
    assert_eq!(
        decode(&e.done()).unwrap(),
        Message::RegShoRestriction(parse::RegShoRestriction {
            header: hdr(),
            stock: b"AMD     ",
            reg_sho_action: b'1',
        })
    );
}

#[test]
fn market_participant_position() {
    // §1.2.4: MPID, Stock, Primary Market Maker, Mode, State.
    let mut e = Enc::new(b'L', &hdr());
    e.bytes(b"NSDQ")
        .bytes(b"MSFT    ")
        .u8(b'Y')
        .u8(b'S')
        .u8(b'W');
    assert_eq!(
        decode(&e.done()).unwrap(),
        Message::MarketParticipantPosition(parse::MarketParticipantPosition {
            header: hdr(),
            mpid: b"NSDQ",
            stock: b"MSFT    ",
            primary_market_maker: b'Y',
            market_maker_mode: b'S',
            market_participant_state: b'W',
        })
    );
}

#[test]
fn mwcb_messages() {
    // §1.2.5.1: Levels 1-3 as Price (8) — 8 bytes, 8 implied decimals.
    let mut e = Enc::new(b'V', &hdr());
    e.u64(280_000_000_000)
        .u64(265_000_000_000)
        .u64(240_000_000_000);
    assert_eq!(
        decode(&e.done()).unwrap(),
        Message::MwcbDeclineLevel(parse::MwcbDeclineLevel {
            header: hdr(),
            level1: Price8(280_000_000_000),
            level2: Price8(265_000_000_000),
            level3: Price8(240_000_000_000),
        })
    );
    // §1.2.5.2: Breached Level @11.
    let mut e = Enc::new(b'W', &hdr());
    e.u8(b'2');
    assert_eq!(
        decode(&e.done()).unwrap(),
        Message::MwcbStatus(parse::MwcbStatus {
            header: hdr(),
            breached_level: b'2',
        })
    );
}

#[test]
fn ipo_quoting_period_update() {
    // §1.2.6: Stock, Release Time (seconds), Qualifier, IPO Price.
    let mut e = Enc::new(b'K', &hdr());
    e.bytes(b"NEWCO   ")
        .u32(9 * 3600 + 45 * 60)
        .u8(b'A')
        .u32(170_000);
    assert_eq!(
        decode(&e.done()).unwrap(),
        Message::IpoQuotingPeriodUpdate(parse::IpoQuotingPeriodUpdate {
            header: hdr(),
            stock: b"NEWCO   ",
            release_time: 35_100,
            release_qualifier: b'A',
            ipo_price: Price4(170_000),
        })
    );
}

#[test]
fn luld_auction_collar() {
    // §1.2.7: Stock, Reference, Upper, Lower, Extension.
    let mut e = Enc::new(b'J', &hdr());
    e.bytes(b"TSLA    ")
        .u32(4_000_000)
        .u32(4_200_000)
        .u32(3_800_000)
        .u32(2);
    assert_eq!(
        decode(&e.done()).unwrap(),
        Message::LuldAuctionCollar(parse::LuldAuctionCollar {
            header: hdr(),
            stock: b"TSLA    ",
            reference_price: Price4(4_000_000),
            upper_collar: Price4(4_200_000),
            lower_collar: Price4(3_800_000),
            extension: 2,
        })
    );
}

#[test]
fn operational_halt() {
    // §1.2.8: Stock, Market Code, Halt Action. Lowercase "h" type.
    let mut e = Enc::new(b'h', &hdr());
    e.bytes(b"SPY     ").u8(b'X').u8(b'H');
    assert_eq!(
        decode(&e.done()).unwrap(),
        Message::OperationalHalt(parse::OperationalHalt {
            header: hdr(),
            stock: b"SPY     ",
            market_code: b'X',
            halt_action: b'H',
        })
    );
}

#[test]
fn add_order_without_and_with_mpid() {
    // §1.3.1: Order Reference, Buy/Sell, Shares, Stock, Price.
    let mut e = Enc::new(b'A', &hdr());
    e.u64(0xDEAD_BEEF_0102_0304)
        .u8(b'B')
        .u32(250)
        .bytes(b"NFLX    ")
        .u32(3_275_500);
    assert_eq!(
        decode(&e.done()).unwrap(),
        Message::AddOrder(parse::AddOrder {
            header: hdr(),
            order_ref: 0xDEAD_BEEF_0102_0304,
            side: Side::Buy,
            shares: 250,
            stock: b"NFLX    ",
            price: Price4(3_275_500),
            attribution: None,
        })
    );
    // §1.3.2: same + Attribution @36.
    let mut e = Enc::new(b'F', &hdr());
    e.u64(77)
        .u8(b'S')
        .u32(100)
        .bytes(b"NFLX    ")
        .u32(3_280_000)
        .bytes(b"JPMS");
    assert_eq!(
        decode(&e.done()).unwrap(),
        Message::AddOrder(parse::AddOrder {
            header: hdr(),
            order_ref: 77,
            side: Side::Sell,
            shares: 100,
            stock: b"NFLX    ",
            price: Price4(3_280_000),
            attribution: Some(b"JPMS"),
        })
    );
}

#[test]
fn order_executed_plain_and_with_price() {
    // §1.4.1: Order Reference, Executed Shares, Match Number.
    let mut e = Enc::new(b'E', &hdr());
    e.u64(42).u32(300).u64(0x0102_0304_0506_0708);
    assert_eq!(
        decode(&e.done()).unwrap(),
        Message::OrderExecuted(parse::OrderExecuted {
            header: hdr(),
            order_ref: 42,
            executed_shares: 300,
            match_number: 0x0102_0304_0506_0708,
        })
    );
    // §1.4.2: adds Printable @31 and Execution Price @32.
    let mut e = Enc::new(b'C', &hdr());
    e.u64(42).u32(50).u64(999).u8(b'N').u32(1_234_500);
    assert_eq!(
        decode(&e.done()).unwrap(),
        Message::OrderExecutedWithPrice(parse::OrderExecutedWithPrice {
            header: hdr(),
            order_ref: 42,
            executed_shares: 50,
            match_number: 999,
            printable: b'N',
            execution_price: Price4(1_234_500),
        })
    );
}

#[test]
fn order_cancel_delete_replace() {
    // §1.4.3: Order Reference, Cancelled Shares.
    let mut e = Enc::new(b'X', &hdr());
    e.u64(7).u32(25);
    assert_eq!(
        decode(&e.done()).unwrap(),
        Message::OrderCancel(parse::OrderCancel {
            header: hdr(),
            order_ref: 7,
            cancelled_shares: 25,
        })
    );
    // §1.4.4: Order Reference only.
    let mut e = Enc::new(b'D', &hdr());
    e.u64(7);
    assert_eq!(
        decode(&e.done()).unwrap(),
        Message::OrderDelete(parse::OrderDelete {
            header: hdr(),
            order_ref: 7,
        })
    );
    // §1.4.5: Original Reference, New Reference, Shares, Price.
    let mut e = Enc::new(b'U', &hdr());
    e.u64(7).u64(8).u32(500).u32(2_973_700);
    assert_eq!(
        decode(&e.done()).unwrap(),
        Message::OrderReplace(parse::OrderReplace {
            header: hdr(),
            original_order_ref: 7,
            new_order_ref: 8,
            shares: 500,
            price: Price4(2_973_700),
        })
    );
}

#[test]
fn trade_messages() {
    // §1.5.1: Order Reference, Buy/Sell, Shares, Stock, Price, Match.
    let mut e = Enc::new(b'P', &hdr());
    e.u64(0)
        .u8(b'B')
        .u32(88)
        .bytes(b"TSLA    ")
        .u32(4_318_100)
        .u64(17_907);
    assert_eq!(
        decode(&e.done()).unwrap(),
        Message::Trade(parse::Trade {
            header: hdr(),
            order_ref: 0,
            side: Side::Buy,
            shares: 88,
            stock: b"TSLA    ",
            price: Price4(4_318_100),
            match_number: 17_907,
        })
    );
    // §1.5.2: Shares is EIGHT bytes here, then Stock, Cross Price, Match,
    // Cross Type.
    let mut e = Enc::new(b'Q', &hdr());
    e.u64(1_500_000)
        .bytes(b"AAPL    ")
        .u32(2_894_100)
        .u64(555)
        .u8(b'O');
    assert_eq!(
        decode(&e.done()).unwrap(),
        Message::CrossTrade(parse::CrossTrade {
            header: hdr(),
            shares: 1_500_000,
            stock: b"AAPL    ",
            cross_price: Price4(2_894_100),
            match_number: 555,
            cross_type: b'O',
        })
    );
    // §1.5.3: Match Number only.
    let mut e = Enc::new(b'B', &hdr());
    e.u64(555);
    assert_eq!(
        decode(&e.done()).unwrap(),
        Message::BrokenTrade(parse::BrokenTrade {
            header: hdr(),
            match_number: 555,
        })
    );
}

#[test]
fn noii() {
    // §1.6: Paired, Imbalance, Direction, Stock, Far, Near, Current
    // Reference, Cross Type, Price Variation Indicator.
    let mut e = Enc::new(b'I', &hdr());
    e.u64(10_000)
        .u64(2_500)
        .u8(b'B')
        .bytes(b"GOOGL   ")
        .u32(13_500_000)
        .u32(13_550_000)
        .u32(13_545_700)
        .u8(b'C')
        .u8(b'L');
    assert_eq!(
        decode(&e.done()).unwrap(),
        Message::Noii(parse::Noii {
            header: hdr(),
            paired_shares: 10_000,
            imbalance_shares: 2_500,
            imbalance_direction: b'B',
            stock: b"GOOGL   ",
            far_price: Price4(13_500_000),
            near_price: Price4(13_550_000),
            current_reference_price: Price4(13_545_700),
            cross_type: b'C',
            price_variation_indicator: b'L',
        })
    );
}

#[test]
fn rpii() {
    // §1.7: Stock, Interest Flag.
    let mut e = Enc::new(b'N', &hdr());
    e.bytes(b"SPY     ").u8(b'A');
    assert_eq!(
        decode(&e.done()).unwrap(),
        Message::RetailPriceImprovement(parse::RetailPriceImprovement {
            header: hdr(),
            stock: b"SPY     ",
            interest_flag: b'A',
        })
    );
}

#[test]
fn direct_listing_capital_raise() {
    // §1.8: Stock, Open Eligibility, Min, Max, Near Execution Price, Near
    // Execution Time (8 bytes), Lower Collar, Upper Collar.
    let mut e = Enc::new(b'O', &hdr());
    e.bytes(b"DLCR    ")
        .u8(b'Y')
        .u32(160_000)
        .u32(3_600_000)
        .u32(2_000_000)
        .u64(34_200_000_000_000)
        .u32(1_800_000)
        .u32(2_200_000);
    assert_eq!(
        decode(&e.done()).unwrap(),
        Message::DirectListingCapitalRaise(parse::DirectListingCapitalRaise {
            header: hdr(),
            stock: b"DLCR    ",
            open_eligibility_status: b'Y',
            minimum_allowable_price: Price4(160_000),
            maximum_allowable_price: Price4(3_600_000),
            near_execution_price: Price4(2_000_000),
            near_execution_time: 34_200_000_000_000,
            lower_price_range_collar: Price4(1_800_000),
            upper_price_range_collar: Price4(2_200_000),
        })
    );
}

#[test]
fn header_decodes_max_48_bit_timestamp() {
    // All-ones timestamp must not touch neighbouring fields.
    let h = Header {
        stock_locate: u16::MAX,
        tracking_number: 0,
        timestamp: 0x0000_FFFF_FFFF_FFFF,
    };
    let mut e = Enc::new(b'D', &h);
    e.u64(1);
    let Message::OrderDelete(m) = decode(&e.done()).unwrap() else {
        panic!("wrong variant");
    };
    assert_eq!(m.header, h);
    assert_eq!(m.order_ref, 1);
}

#[test]
fn error_paths() {
    assert_eq!(decode(&[]), Err(ParseError::Empty));
    assert_eq!(
        decode(&[b'z'; 12]),
        Err(ParseError::UnknownType { ty: b'z' })
    );

    // One byte short and one byte long both fail: exact length only.
    let mut e = Enc::new(b'D', &hdr());
    e.u64(1);
    let good = e.done();
    assert_eq!(
        decode(&good[..good.len() - 1]),
        Err(ParseError::WrongLength {
            ty: b'D',
            len: 18,
            expected: 19,
        })
    );
    let mut long = good.clone();
    long.push(0);
    assert_eq!(
        decode(&long),
        Err(ParseError::WrongLength {
            ty: b'D',
            len: 20,
            expected: 19,
        })
    );

    // Invalid Buy/Sell Indicator on an add.
    let mut e = Enc::new(b'A', &hdr());
    e.u64(1).u8(b'X').u32(100).bytes(b"AAPL    ").u32(1_000_000);
    assert_eq!(
        decode(&e.done()),
        Err(ParseError::InvalidCode {
            ty: b'A',
            offset: 19,
            value: b'X',
        })
    );

    // Invalid System Event code.
    let mut e = Enc::new(b'S', &hdr());
    e.u8(b'Z');
    assert_eq!(
        decode(&e.done()),
        Err(ParseError::InvalidCode {
            ty: b'S',
            offset: 11,
            value: b'Z',
        })
    );

    // Invalid Trading State.
    let mut e = Enc::new(b'H', &hdr());
    e.bytes(b"AAPL    ").u8(b'Z').u8(b' ').bytes(b"    ");
    assert_eq!(
        decode(&e.done()),
        Err(ParseError::InvalidCode {
            ty: b'H',
            offset: 19,
            value: b'Z',
        })
    );
}
