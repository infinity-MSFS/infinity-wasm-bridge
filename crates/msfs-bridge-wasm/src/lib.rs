//!
//! ```rust,ignore
//! use msfs_bridge_wasm::{Bridge, BridgeConfig, CommBusBackend};
//! use serde_json::{json, Value};
//!
//! // Implement CommBusBackend for your MSFS SDK bindings
//! struct MyCommBus;
//! impl CommBusBackend for MyCommBus {
//!     type Error = String;
//!     type Subscription = MySubscription;
//!
//!     fn subscribe(
//!         event: &str,
//!         callback: impl Fn(&str) + 'static,
//!     ) -> Result<Self::Subscription, Self::Error> {
//!         // ... wire to msfs::comm_bus
//!     }
//!
//!     fn call(event: &str, data: &str) -> Result<(), Self::Error> {
//!         // ... wire to msfs::comm_bus::call
//!     }
//! }
//!
//! let bridge = Bridge::<MyCommBus>::new(
//!     BridgeConfig::new("myaddon/bridge_call", "myaddon/bridge_resp"),
//!     |name, payload| {
//!         match name {
//!             Some("get_state") => Ok(json!({"temp": 42})),
//!             _ => Err("unknown command".into()),
//!         }
//!     },
//! )?;
//!
//! // Fire-and-forget event to host
//! bridge.emit("state_changed", json!({"phase": "cruise"}))?;
//! ```

mod backend;
mod bridge;

pub use backend::CommBusBackend;
pub use bridge::{Bridge, BridgeConfig, BridgeHandler, Router};
pub use msfs_bridge_wire::{BridgeError, ErrorKind};
