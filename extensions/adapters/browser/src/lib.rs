//! Browser input is an ordinary Protocol boundary; replay remains generic.

#![forbid(unsafe_code)]

mod coalescer;
mod protocol;

pub use protocol::{
    BrowserEvent, BrowserProtocolError, KeyboardKind, Modifiers, PointerKind, PointerType,
    decode_event, encode_event,
};

pub const BROWSER_ADAPTER_ID: &str = "ato.browser@1";
pub const BROWSER_PROTOCOL_ID: &str = "ato.browser@1";
