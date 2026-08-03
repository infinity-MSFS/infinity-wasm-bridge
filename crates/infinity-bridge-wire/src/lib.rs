#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

mod error;
mod msg;

pub use error::{BridgeError, ErrorKind};
pub use msg::{AckPayload, CmdPayload, EventPayload, HelloPayload, WireMsg};

pub const PROTOCOL_VERSION: u32 = 1;

/// Event name a relay emits (gauge → host) once its CommBus link is bound and
/// it can actually reach the WASM module behind it — a socket being open only
/// proves the *Coherent* half is alive.
///
/// The host consumes this rather than fanning it out to event subscribers, and
/// prefers ready clients when dispatching commands. Relays predating this
/// event never send it, so "no client has ever reported ready" must be treated
/// as "readiness unknown", not as "nothing is reachable".
pub const READY_EVENT: &str = "__bridge_ready";
