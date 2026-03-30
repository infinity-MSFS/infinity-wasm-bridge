#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

mod error;
mod msg;

pub use error::{BridgeError, ErrorKind};
pub use msg::{AckPayload, CmdPayload, EventPayload, HelloPayload, WireMsg};

pub const PROTOCOL_VERSION: u32 = 1;
