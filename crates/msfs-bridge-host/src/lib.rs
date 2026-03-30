//! ```rust,ignore
//! use msfs_bridge_host::{BridgeServer, ServerConfig};
//! use serde_json::json;
//! use std::time::Duration;
//!
//! #[tokio::main]
//! async fn main() {
//!     let server = BridgeServer::start(
//!         ServerConfig::new("127.0.0.1:6969", "/bridge")
//!     ).await.unwrap();
//!
//!     // Wait for a gauge to connect
//!     server.wait_connected().await;
//!
//!     // Send a command and await the response
//!     let response = server
//!         .command("get_state", json!({}), Duration::from_secs(3))
//!         .await
//!         .unwrap();
//!
//!     // Fire-and-forget event to all connected gauges
//!     server.emit("config_updated", json!({"livery": "AA"})).unwrap();
//!
//!     // Receive events from gauges
//!     let mut events = server.subscribe_events();
//!     while let Ok(event) = events.recv().await {
//!         println!("Event: {} = {:?}", event.name, event.data);
//!     }
//! }
//! ```
//!
//! ## Integration with Tauri
//!
//! The server is framework-agnostic. For Tauri, spawn it on the tokio
//! runtime and wire `subscribe_events()` to `app.emit()`:
//!
//! ```rust,ignore
//! let server = BridgeServer::start(config).await?;
//! let app_handle = app.handle().clone();
//! let mut events = server.subscribe_events();
//!
//! tokio::spawn(async move {
//!     while let Ok(event) = events.recv().await {
//!         app_handle.emit(&format!("bridge:{}", event.name), &event.data).ok();
//!     }
//! });
//! ```

mod client;
mod hub;
mod server;

pub use hub::ClientInfo;
pub use msfs_bridge_wire::{BridgeError, ErrorKind, EventPayload, HelloPayload};
pub use server::{BridgeServer, ServerConfig};
