//! # Example: Host Application
//!
//! A standalone configurator application that demonstrates:
//! - Starting the bridge server
//! - Waiting for a gauge to connect
//! - Sending commands and handling responses
//! - Receiving fire-and-forget events from the gauge
//! - Monitoring connection status
//!
//! ## Running
//!
//! ```bash
//! # Terminal 1: Start the host app
//! cargo run --bin host-app
//!
//! # Terminal 2: Start the fake gauge (simulates the JS relay + WASM gauge)
//! cargo run --bin fake-gauge
//! ```
//!
//! The host app will:
//! 1. Start a WebSocket server on ws://127.0.0.1:9876/bridge
//! 2. Wait for a gauge to connect
//! 3. Send an "equip_query" command
//! 4. Send an "equip_cmd" command to toggle equipment
//! 5. Send a fire-and-forget event
//! 6. Listen for events from the gauge
//! 7. Demonstrate error handling (timeout, no clients, application errors)

use msfs_bridge_host::{BridgeServer, ServerConfig};
use msfs_bridge_wire::ErrorKind;
use serde_json::json;
use std::time::Duration;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("═══════════════════════════════════════════════════════════");
    println!("  msfs-bridge example: Host Application (Configurator)");
    println!("═══════════════════════════════════════════════════════════");
    println!();

    // ── 1. Start the server ──────────────────────────────────────

    let config = ServerConfig {
        bind_addr: "127.0.0.1:9876".into(),
        ws_path: "/bridge".into(),
        event_capacity: 256,
        ping_interval: Duration::from_secs(5),
        ping_timeout: Duration::from_secs(15),
    };

    let server = BridgeServer::start(config).await?;
    println!("[host] Bridge server listening on ws://127.0.0.1:9876/bridge");
    println!("[host] Waiting for a gauge to connect...");
    println!("[host] (run `cargo run --bin fake-gauge` in another terminal)");
    println!();

    // ── 2. Monitor connection status in background ───────────────

    let mut status = server.connection_status();
    tokio::spawn(async move {
        while status.changed().await.is_ok() {
            let connected = *status.borrow();
            println!(
                "[host] Connection status changed: {}",
                if connected {
                    "CONNECTED"
                } else {
                    "DISCONNECTED"
                }
            );
        }
    });

    // ── 3. Subscribe to events from gauges in background ─────────

    let mut events = server.subscribe_events();
    tokio::spawn(async move {
        while let Ok(event) = events.recv().await {
            println!(
                "[host] ← Event received: name={}, data={}",
                event.name, event.data
            );
        }
    });

    // ── 4. Demonstrate: command before any gauge connects ────────

    println!("[host] Attempting command before gauge connects (should fail)...");
    match server
        .command("equip_query", json!({}), Duration::from_secs(1))
        .await
    {
        Ok(_) => println!("[host]   Unexpected success"),
        Err(e) if e.kind() == ErrorKind::NoClients => {
            println!("[host]   ✓ Got expected NoClients error: {}", e.message());
        }
        Err(e) => println!("[host]   ✗ Unexpected error: {e}"),
    }
    println!();

    // ── 5. Wait for a gauge to connect ───────────────────────────

    server.wait_connected().await;

    // Brief pause to let the hello handshake complete
    tokio::time::sleep(Duration::from_millis(200)).await;

    let clients = server.clients().await;
    println!("[host] Connected clients:");
    for client in &clients {
        let hello = client
            .hello
            .as_ref()
            .map(|h| {
                format!(
                    "client={}, aircraft={}, session={}",
                    h.client.as_deref().unwrap_or("?"),
                    h.aircraft.as_deref().unwrap_or("?"),
                    h.session.as_deref().unwrap_or("?"),
                )
            })
            .unwrap_or_else(|| "(no hello yet)".into());
        println!("[host]   id={} {}", client.id, hello);
    }
    println!();

    // ── 6. Send equip_query command ──────────────────────────────

    println!("[host] → Sending 'equip_query' command...");
    match server
        .command("equip_query", json!({}), Duration::from_secs(3))
        .await
    {
        Ok(response) => {
            let entries = response
                .get("entries")
                .and_then(|v| v.as_array())
                .map(|a| a.len())
                .unwrap_or(0);
            println!("[host] ← Response: {entries} equipment entries");
            // Pretty-print first 3 entries
            if let Some(arr) = response.get("entries").and_then(|v| v.as_array()) {
                for entry in arr.iter().take(3) {
                    println!(
                        "[host]     idx={} label={:12} available={} active={}",
                        entry.get("idx").and_then(|v| v.as_u64()).unwrap_or(0),
                        entry.get("label").and_then(|v| v.as_str()).unwrap_or("?"),
                        entry
                            .get("available")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false),
                        entry
                            .get("active")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false),
                    );
                }
                if arr.len() > 3 {
                    println!("[host]     ... and {} more", arr.len() - 3);
                }
            }
        }
        Err(e) => println!("[host] ✗ Command failed: {e}"),
    }
    println!();

    // ── 7. Send equip_cmd command ────────────────────────────────

    println!("[host] → Sending 'equip_cmd' (toggle idx=1)...");
    match server
        .command(
            "equip_cmd",
            json!({"equipIdx": 1, "cmd": 2}),
            Duration::from_secs(3),
        )
        .await
    {
        Ok(response) => {
            println!(
                "[host] ← Toggle successful, response keys: {:?}",
                response.as_object().map(|o| o.keys().collect::<Vec<_>>())
            );
        }
        Err(e) => println!("[host] ✗ Command failed: {e}"),
    }
    println!();

    // ── 8. Send fire-and-forget event to gauge ───────────────────

    println!("[host] → Sending 'config_updated' event (fire-and-forget)...");
    match server
        .emit(
            "config_updated",
            json!({"livery": "American Airlines", "year": 1985}),
        )
        .await
    {
        Ok(()) => println!("[host]   ✓ Event dispatched"),
        Err(e) => println!("[host]   ✗ Event failed: {e}"),
    }
    println!();

    // ── 9. Demonstrate timeout ───────────────────────────────────

    println!("[host] → Sending 'slow_command' (will timeout in 500ms)...");
    match server
        .command("slow_command", json!({}), Duration::from_millis(500))
        .await
    {
        Ok(_) => println!("[host]   Unexpected success"),
        Err(e) if e.kind() == ErrorKind::Timeout => {
            println!("[host]   ✓ Got expected Timeout error: {}", e.message());
        }
        Err(e) => println!("[host]   Got error: {e}"),
    }
    println!();

    // ── 10. Demonstrate application error ────────────────────────

    println!("[host] → Sending 'nonexistent_command'...");
    match server
        .command("nonexistent_command", json!({}), Duration::from_secs(3))
        .await
    {
        Ok(_) => println!("[host]   Unexpected success"),
        Err(e) if e.kind() == ErrorKind::Application => {
            println!("[host]   ✓ Got expected Application error: {}", e.message());
        }
        Err(e) => println!("[host]   Got error: {e}"),
    }
    println!();

    // ── 11. Keep running, listen for events ──────────────────────

    println!("[host] All demonstrations complete. Listening for events...");
    println!("[host] Press Ctrl+C to exit.");
    println!();

    tokio::signal::ctrl_c().await?;
    println!("\n[host] Shutting down.");

    Ok(())
}
