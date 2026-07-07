//! Zero-copy decode of ITCH 5.0 message payloads into typed messages.
//!
//! Every field offset, length, and value convention below is taken from the
//! committed spec (`spec/itch50_spec.txt`); each struct cites its section.
//! Payloads are the framed bodies yielded by [`crate::frame::Messages`] — the
//! 2-byte length prefix is transport framing, not part of any layout here.
//!
//! Conventions (spec "Data Types"):
//! - all integers are big-endian and unsigned;
//! - alpha fields are ASCII, left-justified, right-padded with spaces —
//!   decoded as borrowed fixed-size byte arrays, never copied or re-encoded;
//! - prices are fixed-point integers: `Price (4)` has 4 implied decimal
//!   places, `Price (8)` has 8 ([`Price4`], [`Price8`]);
//! - timestamps are 6-byte (48-bit) nanoseconds since midnight.
//!
//! Validation policy: fields the order book depends on are decoded strictly
//! (exact payload length per type; [`Side`], [`EventCode`], [`TradingState`]
//! reject undefined values). Purely informational coded fields (market
//! category, Reg SHO action, cross type, …) are passed through as raw bytes:
//! Nasdaq adds codes to those over time (the spec's revision log added NOII
//! imbalance direction "P" in 2023) and a book replay must not fail on them.

/// An 8-byte stock symbol, ASCII, right-padded with spaces.
pub type Stock = [u8; 8];

/// A 4-byte market participant identifier, ASCII, right-padded with spaces.
pub type Mpid = [u8; 4];

/// Strips the spec's trailing-space padding from an alpha field.
pub fn trim_padding(field: &[u8]) -> &[u8] {
    let end = field.iter().rposition(|&b| b != b' ').map_or(0, |i| i + 1);
    &field[..end]
}

/// A fixed-point price with 4 implied decimal places (spec "Data Types"):
/// raw `181850` means 18.1850. Ordering on the raw integer is exactly price
/// ordering, so books compare `Price4` directly and never touch floats.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Price4(pub u32);

impl Price4 {
    /// Splits into (whole dollars, 1/10000ths) for display; no floats.
    pub fn split(self) -> (u32, u32) {
        (self.0 / 10_000, self.0 % 10_000)
    }
}

/// A fixed-point price with 8 implied decimal places, used only by the MWCB
/// Decline Level message (spec §1.2.5.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Price8(pub u64);

/// Buy/Sell Indicator (spec §1.3.1): "B" = buy, "S" = sell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Side {
    Buy,
    Sell,
}

/// System Event Codes (spec §1.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventCode {
    /// "O" — Start of Messages: first message of the trading day.
    StartOfMessages,
    /// "S" — Start of System hours: Nasdaq is open and accepting orders.
    StartOfSystemHours,
    /// "Q" — Start of Market hours: market-hours orders may now execute.
    StartOfMarketHours,
    /// "M" — End of Market hours.
    EndOfMarketHours,
    /// "E" — End of System hours.
    EndOfSystemHours,
    /// "C" — End of Messages: last message of the trading day.
    EndOfMessages,
}

/// Trading State from the Stock Trading Action message (spec §1.2.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TradingState {
    /// "H" — Halted across all U.S. equity markets / SROs.
    Halted,
    /// "P" — Paused across all U.S. equity markets / SROs (Nasdaq-listed).
    Paused,
    /// "Q" — Quotation only period for cross-SRO halt or pause.
    QuotationOnly,
    /// "T" — Trading on Nasdaq.
    Trading,
}

/// The uniform header shared by every ITCH 5.0 message: Stock Locate
/// (offset 1, 2 bytes), Tracking Number (offset 3, 2 bytes), Timestamp
/// (offset 5, 6 bytes, nanoseconds since midnight). The message type at
/// offset 0 is represented by the [`Message`] variant itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    pub stock_locate: u16,
    pub tracking_number: u16,
    /// Nanoseconds since midnight, decoded from the 48-bit field.
    pub timestamp: u64,
}

/// System Event Message, type "S", 12 bytes (spec §1.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SystemEvent {
    pub header: Header,
    pub event_code: EventCode,
}

/// Stock Directory, type "R", 39 bytes (spec §1.2.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StockDirectory<'a> {
    pub header: Header,
    pub stock: &'a Stock,
    pub market_category: u8,
    pub financial_status: u8,
    pub round_lot_size: u32,
    pub round_lots_only: u8,
    pub issue_classification: u8,
    pub issue_subtype: &'a [u8; 2],
    pub authenticity: u8,
    pub short_sale_threshold: u8,
    pub ipo_flag: u8,
    pub luld_reference_price_tier: u8,
    pub etp_flag: u8,
    pub etp_leverage_factor: u32,
    pub inverse_indicator: u8,
}

/// Stock Trading Action, type "H", 25 bytes (spec §1.2.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StockTradingAction<'a> {
    pub header: Header,
    pub stock: &'a Stock,
    pub trading_state: TradingState,
    pub reserved: u8,
    pub reason: &'a [u8; 4],
}

/// Reg SHO Short Sale Price Test Restricted Indicator, type "Y", 20 bytes
/// (spec §1.2.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RegShoRestriction<'a> {
    pub header: Header,
    pub stock: &'a Stock,
    /// "0" / "1" / "2" per spec; informational, kept raw.
    pub reg_sho_action: u8,
}

/// Market Participant Position, type "L", 26 bytes (spec §1.2.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MarketParticipantPosition<'a> {
    pub header: Header,
    pub mpid: &'a Mpid,
    pub stock: &'a Stock,
    pub primary_market_maker: u8,
    pub market_maker_mode: u8,
    pub market_participant_state: u8,
}

/// MWCB Decline Level, type "V", 35 bytes (spec §1.2.5.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MwcbDeclineLevel {
    pub header: Header,
    pub level1: Price8,
    pub level2: Price8,
    pub level3: Price8,
}

/// MWCB Status, type "W", 12 bytes (spec §1.2.5.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MwcbStatus {
    pub header: Header,
    pub breached_level: u8,
}

/// IPO Quoting Period Update, type "K", 28 bytes (spec §1.2.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IpoQuotingPeriodUpdate<'a> {
    pub header: Header,
    pub stock: &'a Stock,
    /// Seconds since midnight, to the nearest second.
    pub release_time: u32,
    pub release_qualifier: u8,
    pub ipo_price: Price4,
}

/// LULD Auction Collar, type "J", 35 bytes (spec §1.2.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LuldAuctionCollar<'a> {
    pub header: Header,
    pub stock: &'a Stock,
    pub reference_price: Price4,
    pub upper_collar: Price4,
    pub lower_collar: Price4,
    pub extension: u32,
}

/// Operational Halt, type "h", 21 bytes (spec §1.2.8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OperationalHalt<'a> {
    pub header: Header,
    pub stock: &'a Stock,
    pub market_code: u8,
    pub halt_action: u8,
}

/// Add Order, types "A" (no MPID, 36 bytes, spec §1.3.1) and "F" (with MPID
/// attribution, 40 bytes, spec §1.3.2). The two layouts are identical except
/// that "F" appends the 4-byte Attribution field, so they share one struct;
/// `attribution` is `Some` exactly for "F".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AddOrder<'a> {
    pub header: Header,
    /// Day-unique reference number assigned to the order at receipt.
    pub order_ref: u64,
    pub side: Side,
    pub shares: u32,
    pub stock: &'a Stock,
    pub price: Price4,
    pub attribution: Option<&'a Mpid>,
}

/// Order Executed, type "E", 31 bytes (spec §1.4.1): an execution against a
/// resting order at its display price. Effects on the same order are
/// cumulative; at zero remaining shares the order is dead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OrderExecuted {
    pub header: Header,
    pub order_ref: u64,
    pub executed_shares: u32,
    pub match_number: u64,
}

/// Order Executed With Price, type "C", 36 bytes (spec §1.4.2): like "E" but
/// executed at a price different from the display price, so the message
/// carries the execution price and a printable flag ("N" executions are
/// excluded from time-and-sales / volume to avoid double counting). The book
/// effect is identical to "E": deduct `executed_shares` from the resting
/// order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OrderExecutedWithPrice {
    pub header: Header,
    pub order_ref: u64,
    pub executed_shares: u32,
    pub match_number: u64,
    /// "Y" or "N"; informational, kept raw.
    pub printable: u8,
    pub execution_price: Price4,
}

/// Order Cancel, type "X", 23 bytes (spec §1.4.3): a partial cancellation
/// reducing the display size of a resting order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OrderCancel {
    pub header: Header,
    pub order_ref: u64,
    pub cancelled_shares: u32,
}

/// Order Delete, type "D", 19 bytes (spec §1.4.4): all remaining shares are
/// no longer accessible; the order must be removed from the book.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OrderDelete {
    pub header: Header,
    pub order_ref: u64,
}

/// Order Replace, type "U", 35 bytes (spec §1.4.5): the original order is
/// retired and a new order reference number takes over with new shares and
/// price. Side, stock, and attribution carry over from the original add and
/// are not present in the message; queue priority does NOT carry over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OrderReplace {
    pub header: Header,
    pub original_order_ref: u64,
    pub new_order_ref: u64,
    /// The new total displayed quantity (not a delta).
    pub shares: u32,
    pub price: Price4,
}

/// Trade (Non-Cross), type "P", 44 bytes (spec §1.5.1): an execution against
/// non-displayable interest. Does not affect the displayed book. The order
/// reference number is zero-filled (per spec, effective 2010) and the side
/// is always "B" (effective 2014).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Trade<'a> {
    pub header: Header,
    pub order_ref: u64,
    pub side: Side,
    pub shares: u32,
    pub stock: &'a Stock,
    pub price: Price4,
    pub match_number: u64,
}

/// Cross Trade, type "Q", 40 bytes (spec §1.5.2): bulk print for a completed
/// cross. Note the 8-byte Shares field, unlike the 4-byte field elsewhere.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CrossTrade<'a> {
    pub header: Header,
    pub shares: u64,
    pub stock: &'a Stock,
    pub cross_price: Price4,
    pub match_number: u64,
    /// "O" opening / "C" closing / "H" IPO-halted; informational, kept raw.
    pub cross_type: u8,
}

/// Broken Trade, type "B", 19 bytes (spec §1.5.3). No book impact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BrokenTrade {
    pub header: Header,
    pub match_number: u64,
}

/// Net Order Imbalance Indicator, type "I", 50 bytes (spec §1.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Noii<'a> {
    pub header: Header,
    pub paired_shares: u64,
    pub imbalance_shares: u64,
    pub imbalance_direction: u8,
    pub stock: &'a Stock,
    pub far_price: Price4,
    pub near_price: Price4,
    pub current_reference_price: Price4,
    pub cross_type: u8,
    pub price_variation_indicator: u8,
}

/// Retail Price Improvement Indicator, type "N", 20 bytes (spec §1.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetailPriceImprovement<'a> {
    pub header: Header,
    pub stock: &'a Stock,
    pub interest_flag: u8,
}

/// Direct Listing with Capital Raise Price Discovery, type "O", 48 bytes
/// (spec §1.8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DirectListingCapitalRaise<'a> {
    pub header: Header,
    pub stock: &'a Stock,
    pub open_eligibility_status: u8,
    pub minimum_allowable_price: Price4,
    pub maximum_allowable_price: Price4,
    pub near_execution_price: Price4,
    pub near_execution_time: u64,
    pub lower_price_range_collar: Price4,
    pub upper_price_range_collar: Price4,
}

/// One decoded ITCH 5.0 message. Every type defined by the spec is covered,
/// so a full sample day decodes without hitting `UnknownType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Message<'a> {
    SystemEvent(SystemEvent),
    StockDirectory(StockDirectory<'a>),
    StockTradingAction(StockTradingAction<'a>),
    RegShoRestriction(RegShoRestriction<'a>),
    MarketParticipantPosition(MarketParticipantPosition<'a>),
    MwcbDeclineLevel(MwcbDeclineLevel),
    MwcbStatus(MwcbStatus),
    IpoQuotingPeriodUpdate(IpoQuotingPeriodUpdate<'a>),
    LuldAuctionCollar(LuldAuctionCollar<'a>),
    OperationalHalt(OperationalHalt<'a>),
    AddOrder(AddOrder<'a>),
    OrderExecuted(OrderExecuted),
    OrderExecutedWithPrice(OrderExecutedWithPrice),
    OrderCancel(OrderCancel),
    OrderDelete(OrderDelete),
    OrderReplace(OrderReplace),
    Trade(Trade<'a>),
    CrossTrade(CrossTrade<'a>),
    BrokenTrade(BrokenTrade),
    Noii(Noii<'a>),
    RetailPriceImprovement(RetailPriceImprovement<'a>),
    DirectListingCapitalRaise(DirectListingCapitalRaise<'a>),
}

impl Message<'_> {
    /// The uniform header present on every message.
    pub fn header(&self) -> &Header {
        match self {
            Message::SystemEvent(m) => &m.header,
            Message::StockDirectory(m) => &m.header,
            Message::StockTradingAction(m) => &m.header,
            Message::RegShoRestriction(m) => &m.header,
            Message::MarketParticipantPosition(m) => &m.header,
            Message::MwcbDeclineLevel(m) => &m.header,
            Message::MwcbStatus(m) => &m.header,
            Message::IpoQuotingPeriodUpdate(m) => &m.header,
            Message::LuldAuctionCollar(m) => &m.header,
            Message::OperationalHalt(m) => &m.header,
            Message::AddOrder(m) => &m.header,
            Message::OrderExecuted(m) => &m.header,
            Message::OrderExecutedWithPrice(m) => &m.header,
            Message::OrderCancel(m) => &m.header,
            Message::OrderDelete(m) => &m.header,
            Message::OrderReplace(m) => &m.header,
            Message::Trade(m) => &m.header,
            Message::CrossTrade(m) => &m.header,
            Message::BrokenTrade(m) => &m.header,
            Message::Noii(m) => &m.header,
            Message::RetailPriceImprovement(m) => &m.header,
            Message::DirectListingCapitalRaise(m) => &m.header,
        }
    }
}

/// A decode error. Carries enough to locate the offending byte in a report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseError {
    /// Zero-length payload: no message type byte to dispatch on.
    Empty,
    /// The message type byte is not defined by the spec.
    UnknownType { ty: u8 },
    /// The payload length does not match the fixed length the spec defines
    /// for this message type.
    WrongLength { ty: u8, len: usize, expected: usize },
    /// A strictly-validated coded field (Side, Event Code, Trading State)
    /// holds a value the spec does not define.
    InvalidCode { ty: u8, offset: usize, value: u8 },
}

/// The exact payload length the spec fixes for each message type, or `None`
/// for type bytes the spec does not define. Derived from the last field's
/// offset + length in each message table of `spec/itch50_spec.txt`.
pub const fn message_len(ty: u8) -> Option<usize> {
    Some(match ty {
        b'S' => 12, // §1.1   System Event: Event Code @11+1
        b'R' => 39, // §1.2.1 Stock Directory: Inverse Indicator @38+1
        b'H' => 25, // §1.2.2 Stock Trading Action: Reason @21+4
        b'Y' => 20, // §1.2.3 Reg SHO Restriction: Reg SHO Action @19+1
        b'L' => 26, // §1.2.4 Market Participant Position: State @25+1
        b'V' => 35, // §1.2.5.1 MWCB Decline Level: Level 3 @27+8
        b'W' => 12, // §1.2.5.2 MWCB Status: Breached Level @11+1
        b'K' => 28, // §1.2.6 IPO Quoting Period Update: IPO Price @24+4
        b'J' => 35, // §1.2.7 LULD Auction Collar: Extension @31+4
        b'h' => 21, // §1.2.8 Operational Halt: Halt Action @20+1
        b'A' => 36, // §1.3.1 Add Order: Price @32+4
        b'F' => 40, // §1.3.2 Add Order w/ MPID: Attribution @36+4
        b'E' => 31, // §1.4.1 Order Executed: Match Number @23+8
        b'C' => 36, // §1.4.2 Order Executed w/ Price: Execution Price @32+4
        b'X' => 23, // §1.4.3 Order Cancel: Cancelled Shares @19+4
        b'D' => 19, // §1.4.4 Order Delete: Order Reference Number @11+8
        b'U' => 35, // §1.4.5 Order Replace: Price @31+4
        b'P' => 44, // §1.5.1 Trade (Non-Cross): Match Number @36+8
        b'Q' => 40, // §1.5.2 Cross Trade: Cross Type @39+1
        b'B' => 19, // §1.5.3 Broken Trade: Match Number @11+8
        b'I' => 50, // §1.6   NOII: Price Variation Indicator @49+1
        b'N' => 20, // §1.7   RPII: Interest Flag @19+1
        b'O' => 48, // §1.8   DLCR: Upper Price Range Collar @44+4
        _ => return None,
    })
}

#[inline]
fn u16_at(p: &[u8], off: usize) -> u16 {
    u16::from_be_bytes([p[off], p[off + 1]])
}

#[inline]
fn u32_at(p: &[u8], off: usize) -> u32 {
    u32::from_be_bytes([p[off], p[off + 1], p[off + 2], p[off + 3]])
}

/// The 48-bit big-endian timestamp field, widened to u64.
#[inline]
fn u48_at(p: &[u8], off: usize) -> u64 {
    u64::from_be_bytes([
        0,
        0,
        p[off],
        p[off + 1],
        p[off + 2],
        p[off + 3],
        p[off + 4],
        p[off + 5],
    ])
}

#[inline]
fn u64_at(p: &[u8], off: usize) -> u64 {
    u64::from_be_bytes([
        p[off],
        p[off + 1],
        p[off + 2],
        p[off + 3],
        p[off + 4],
        p[off + 5],
        p[off + 6],
        p[off + 7],
    ])
}

/// Borrows a fixed-size alpha field out of the payload (zero-copy).
#[inline]
fn array_at<const N: usize>(p: &[u8], off: usize) -> &[u8; N] {
    // Callers pass spec-constant offsets into a payload whose exact length
    // `decode` has already checked, so this slice is always in bounds.
    p[off..off + N]
        .try_into()
        .expect("length checked in decode")
}

fn side_at(p: &[u8], off: usize, ty: u8) -> Result<Side, ParseError> {
    match p[off] {
        b'B' => Ok(Side::Buy),
        b'S' => Ok(Side::Sell),
        value => Err(ParseError::InvalidCode {
            ty,
            offset: off,
            value,
        }),
    }
}

/// Decodes one framed payload into a typed [`Message`].
///
/// Zero-copy: alpha fields in the result borrow from `payload`. The payload
/// length must equal the spec's fixed length for the type ([`message_len`]);
/// no trailing bytes are tolerated. Never panics, for any input.
pub fn decode(payload: &[u8]) -> Result<Message<'_>, ParseError> {
    let Some(&ty) = payload.first() else {
        return Err(ParseError::Empty);
    };
    let Some(expected) = message_len(ty) else {
        return Err(ParseError::UnknownType { ty });
    };
    if payload.len() != expected {
        return Err(ParseError::WrongLength {
            ty,
            len: payload.len(),
            expected,
        });
    }
    let p = payload;
    // Uniform header (spec "Message Formats"): Stock Locate @1 (2 bytes),
    // Tracking Number @3 (2 bytes), Timestamp @5 (6 bytes).
    let header = Header {
        stock_locate: u16_at(p, 1),
        tracking_number: u16_at(p, 3),
        timestamp: u48_at(p, 5),
    };
    Ok(match ty {
        // §1.1: Event Code @11.
        b'S' => Message::SystemEvent(SystemEvent {
            header,
            event_code: match p[11] {
                b'O' => EventCode::StartOfMessages,
                b'S' => EventCode::StartOfSystemHours,
                b'Q' => EventCode::StartOfMarketHours,
                b'M' => EventCode::EndOfMarketHours,
                b'E' => EventCode::EndOfSystemHours,
                b'C' => EventCode::EndOfMessages,
                value => {
                    return Err(ParseError::InvalidCode {
                        ty,
                        offset: 11,
                        value,
                    });
                }
            },
        }),
        // §1.2.1: Stock @11, Market Category @19, Financial Status @20,
        // Round Lot Size @21, Round Lots Only @25, Issue Classification @26,
        // Issue Sub-Type @27, Authenticity @29, Short Sale Threshold @30,
        // IPO Flag @31, LULD Reference Price Tier @32, ETP Flag @33,
        // ETP Leverage Factor @34, Inverse Indicator @38.
        b'R' => Message::StockDirectory(StockDirectory {
            header,
            stock: array_at(p, 11),
            market_category: p[19],
            financial_status: p[20],
            round_lot_size: u32_at(p, 21),
            round_lots_only: p[25],
            issue_classification: p[26],
            issue_subtype: array_at(p, 27),
            authenticity: p[29],
            short_sale_threshold: p[30],
            ipo_flag: p[31],
            luld_reference_price_tier: p[32],
            etp_flag: p[33],
            etp_leverage_factor: u32_at(p, 34),
            inverse_indicator: p[38],
        }),
        // §1.2.2: Stock @11, Trading State @19, Reserved @20, Reason @21.
        b'H' => Message::StockTradingAction(StockTradingAction {
            header,
            stock: array_at(p, 11),
            trading_state: match p[19] {
                b'H' => TradingState::Halted,
                b'P' => TradingState::Paused,
                b'Q' => TradingState::QuotationOnly,
                b'T' => TradingState::Trading,
                value => {
                    return Err(ParseError::InvalidCode {
                        ty,
                        offset: 19,
                        value,
                    });
                }
            },
            reserved: p[20],
            reason: array_at(p, 21),
        }),
        // §1.2.3: Stock @11, Reg SHO Action @19.
        b'Y' => Message::RegShoRestriction(RegShoRestriction {
            header,
            stock: array_at(p, 11),
            reg_sho_action: p[19],
        }),
        // §1.2.4: MPID @11, Stock @15, Primary Market Maker @23,
        // Market Maker Mode @24, Market Participant State @25.
        b'L' => Message::MarketParticipantPosition(MarketParticipantPosition {
            header,
            mpid: array_at(p, 11),
            stock: array_at(p, 15),
            primary_market_maker: p[23],
            market_maker_mode: p[24],
            market_participant_state: p[25],
        }),
        // §1.2.5.1: Level 1 @11, Level 2 @19, Level 3 @27, all Price (8).
        b'V' => Message::MwcbDeclineLevel(MwcbDeclineLevel {
            header,
            level1: Price8(u64_at(p, 11)),
            level2: Price8(u64_at(p, 19)),
            level3: Price8(u64_at(p, 27)),
        }),
        // §1.2.5.2: Breached Level @11.
        b'W' => Message::MwcbStatus(MwcbStatus {
            header,
            breached_level: p[11],
        }),
        // §1.2.6: Stock @11, Release Time @19, Qualifier @23, IPO Price @24.
        b'K' => Message::IpoQuotingPeriodUpdate(IpoQuotingPeriodUpdate {
            header,
            stock: array_at(p, 11),
            release_time: u32_at(p, 19),
            release_qualifier: p[23],
            ipo_price: Price4(u32_at(p, 24)),
        }),
        // §1.2.7: Stock @11, Reference @19, Upper @23, Lower @27,
        // Extension @31.
        b'J' => Message::LuldAuctionCollar(LuldAuctionCollar {
            header,
            stock: array_at(p, 11),
            reference_price: Price4(u32_at(p, 19)),
            upper_collar: Price4(u32_at(p, 23)),
            lower_collar: Price4(u32_at(p, 27)),
            extension: u32_at(p, 31),
        }),
        // §1.2.8: Stock @11, Market Code @19, Halt Action @20.
        b'h' => Message::OperationalHalt(OperationalHalt {
            header,
            stock: array_at(p, 11),
            market_code: p[19],
            halt_action: p[20],
        }),
        // §1.3.1 / §1.3.2: Order Reference @11, Buy/Sell @19, Shares @20,
        // Stock @24, Price @32; "F" adds Attribution @36.
        b'A' | b'F' => Message::AddOrder(AddOrder {
            header,
            order_ref: u64_at(p, 11),
            side: side_at(p, 19, ty)?,
            shares: u32_at(p, 20),
            stock: array_at(p, 24),
            price: Price4(u32_at(p, 32)),
            attribution: if ty == b'F' {
                Some(array_at(p, 36))
            } else {
                None
            },
        }),
        // §1.4.1: Order Reference @11, Executed Shares @19, Match @23.
        b'E' => Message::OrderExecuted(OrderExecuted {
            header,
            order_ref: u64_at(p, 11),
            executed_shares: u32_at(p, 19),
            match_number: u64_at(p, 23),
        }),
        // §1.4.2: Order Reference @11, Executed Shares @19, Match @23,
        // Printable @31, Execution Price @32.
        b'C' => Message::OrderExecutedWithPrice(OrderExecutedWithPrice {
            header,
            order_ref: u64_at(p, 11),
            executed_shares: u32_at(p, 19),
            match_number: u64_at(p, 23),
            printable: p[31],
            execution_price: Price4(u32_at(p, 32)),
        }),
        // §1.4.3: Order Reference @11, Cancelled Shares @19.
        b'X' => Message::OrderCancel(OrderCancel {
            header,
            order_ref: u64_at(p, 11),
            cancelled_shares: u32_at(p, 19),
        }),
        // §1.4.4: Order Reference @11.
        b'D' => Message::OrderDelete(OrderDelete {
            header,
            order_ref: u64_at(p, 11),
        }),
        // §1.4.5: Original Order Reference @11, New Order Reference @19,
        // Shares @27, Price @31.
        b'U' => Message::OrderReplace(OrderReplace {
            header,
            original_order_ref: u64_at(p, 11),
            new_order_ref: u64_at(p, 19),
            shares: u32_at(p, 27),
            price: Price4(u32_at(p, 31)),
        }),
        // §1.5.1: Order Reference @11, Buy/Sell @19, Shares @20, Stock @24,
        // Price @32, Match Number @36.
        b'P' => Message::Trade(Trade {
            header,
            order_ref: u64_at(p, 11),
            side: side_at(p, 19, ty)?,
            shares: u32_at(p, 20),
            stock: array_at(p, 24),
            price: Price4(u32_at(p, 32)),
            match_number: u64_at(p, 36),
        }),
        // §1.5.2: Shares @11 (8 bytes), Stock @19, Cross Price @27,
        // Match Number @31, Cross Type @39.
        b'Q' => Message::CrossTrade(CrossTrade {
            header,
            shares: u64_at(p, 11),
            stock: array_at(p, 19),
            cross_price: Price4(u32_at(p, 27)),
            match_number: u64_at(p, 31),
            cross_type: p[39],
        }),
        // §1.5.3: Match Number @11.
        b'B' => Message::BrokenTrade(BrokenTrade {
            header,
            match_number: u64_at(p, 11),
        }),
        // §1.6: Paired Shares @11, Imbalance Shares @19, Direction @27,
        // Stock @28, Far @36, Near @40, Current Reference @44,
        // Cross Type @48, Price Variation Indicator @49.
        b'I' => Message::Noii(Noii {
            header,
            paired_shares: u64_at(p, 11),
            imbalance_shares: u64_at(p, 19),
            imbalance_direction: p[27],
            stock: array_at(p, 28),
            far_price: Price4(u32_at(p, 36)),
            near_price: Price4(u32_at(p, 40)),
            current_reference_price: Price4(u32_at(p, 44)),
            cross_type: p[48],
            price_variation_indicator: p[49],
        }),
        // §1.7: Stock @11, Interest Flag @19.
        b'N' => Message::RetailPriceImprovement(RetailPriceImprovement {
            header,
            stock: array_at(p, 11),
            interest_flag: p[19],
        }),
        // §1.8: Stock @11, Open Eligibility @19, Minimum @20, Maximum @24,
        // Near Execution Price @28, Near Execution Time @32, Lower Collar
        // @40, Upper Collar @44.
        b'O' => Message::DirectListingCapitalRaise(DirectListingCapitalRaise {
            header,
            stock: array_at(p, 11),
            open_eligibility_status: p[19],
            minimum_allowable_price: Price4(u32_at(p, 20)),
            maximum_allowable_price: Price4(u32_at(p, 24)),
            near_execution_price: Price4(u32_at(p, 28)),
            near_execution_time: u64_at(p, 32),
            lower_price_range_collar: Price4(u32_at(p, 40)),
            upper_price_range_collar: Price4(u32_at(p, 44)),
        }),
        // Unreachable: message_len() returned Some above only for the types
        // matched here.
        _ => return Err(ParseError::UnknownType { ty }),
    })
}
