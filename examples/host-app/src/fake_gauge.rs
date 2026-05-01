//! # Example: Fake Gauge
//!
//! Simulates the full JS relay + WASM gauge pipeline as a single WebSocket
//! client. Useful for testing the host application without MSFS running.
//!
//! This demonstrates what the relay gauge and WASM bridge do under the hood:
//! - Connects to the host WebSocket server
//! - Sends a hello handshake
//! - Responds to commands with mock data
//! - Periodically emits fire-and-forget events
//! - Handles ping/pong keepalive
//!
//! ## Running
//!
//! ```bash
//! # Terminal 1: Start the host app first
//! cargo run --bin host-app
//!
//! # Terminal 2: Start the fake gauge
//! cargo run --bin fake-gauge
//! ```

use futures::{SinkExt, StreamExt};
use infinity_bridge_wire::{AckPayload, EventPayload, HelloPayload, WireMsg};
use serde_json::{Value, json};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_tungstenite::{connect_async, tungstenite::Message};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("═══════════════════════════════════════════════════════════");
    println!("  msfs-bridge example: Fake Gauge (simulates relay+WASM)");
    println!("═══════════════════════════════════════════════════════════");
    println!();

    let url = "ws://127.0.0.1:9876/bridge";
    println!("[gauge] Connecting to {url}...");

    let (ws, _) = connect_async(url).await?;
    println!("[gauge] Connected!");

    let (mut ws_tx, mut ws_rx) = ws.split();

    // Single outbound channel — all outbound messages go through here
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<String>();

    // ── Writer task ──────────────────────────────────────────────

    let writer = tokio::spawn(async move {
        while let Some(text) = out_rx.recv().await {
            if ws_tx.send(Message::Text(text.into())).await.is_err() {
                break;
            }
        }
    });

    // ── Send hello ───────────────────────────────────────────────

    let hello = WireMsg::Hello(HelloPayload {
        client: Some("fake-gauge".into()),
        aircraft: Some("DC-10-30".into()),
        tail: Some("N1819U".into()),
        session: Some(format!("test-{}", epoch_secs())),
        v: Some(1),
        meta: None,
    });
    println!("[gauge] → Hello");
    out_tx.send(hello.to_json()?)?;

    // ── Periodic telemetry emitter (WASM→Host events) ────────────

    let out_tx_events = out_tx.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(3));
        interval.tick().await; // skip first immediate tick
        let mut tick = 0u32;
        loop {
            interval.tick().await;
            tick += 1;

            let event = WireMsg::Event(EventPayload::new(
                "telemetry",
                json!({
                    "altitude_ft": 35000 + (tick as i32 * 100 % 2000),
                    "speed_kts": 280 + (tick % 20),
                    "heading_deg": (tick * 15) % 360,
                    "tick": tick,
                }),
            ));

            match event.to_json() {
                Ok(json) => {
                    println!("[gauge] → Telemetry event (tick {tick})");
                    if out_tx_events.send(json).is_err() {
                        break;
                    }
                }
                Err(e) => eprintln!("[gauge] Serialize error: {e}"),
            }
        }
    });

    // ── Reader loop ──────────────────────────────────────────────

    println!("[gauge] Listening for messages from host...");
    println!();

    while let Some(msg) = ws_rx.next().await {
        let text = match msg {
            Ok(Message::Text(t)) => t,
            Ok(Message::Close(f)) => {
                println!("[gauge] Server closed connection: {f:?}");
                break;
            }
            Ok(_) => continue,
            Err(e) => {
                println!("[gauge] WebSocket error: {e}");
                break;
            }
        };

        let wire = match WireMsg::from_json(&text) {
            Ok(w) => w,
            Err(e) => {
                println!("[gauge] !! Parse error: {e}");
                continue;
            }
        };

        match wire {
            // ── Keepalive ────────────────────────────────────────
            WireMsg::Ping { ts } => {
                let pong = WireMsg::Pong { ts };
                if let Ok(j) = pong.to_json() {
                    let _ = out_tx.send(j);
                }
            }

            // ── Commands (host → gauge) ──────────────────────────
            WireMsg::Cmd(cmd) => {
                println!("[gauge] ← Command: name={:?}, id={}", cmd.name, cmd.id);

                let ack = handle_command(&cmd.id, cmd.name.as_deref(), &cmd.payload);

                // Some commands intentionally don't respond (to test timeout)
                if let Some(ack) = ack {
                    if let Ok(j) = ack.to_json() {
                        let _ = out_tx.send(j);
                    }
                }
            }

            // ── Events (host → gauge, fire-and-forget) ──────────
            WireMsg::Event(event) => {
                println!(
                    "[gauge] ← Event from host: name={}, data={}",
                    event.name, event.data
                );
            }

            other => {
                println!("[gauge] ← Other: {other:?}");
            }
        }
    }

    println!("[gauge] Disconnected.");
    drop(out_tx);
    let _ = writer.await;

    Ok(())
}

// ── Command handler (simulates what WASM bridge + Router would do) ───

/// Returns `None` to intentionally not respond (for timeout testing).
fn handle_command(id: &str, name: Option<&str>, payload: &Value) -> Option<WireMsg> {
    match name {
        Some("equip_query") => {
            println!("[gauge]   → Responding with mock equipment state");
            Some(WireMsg::Ack(AckPayload::ok(
                id.into(),
                json!({ "entries": mock_equipment_entries() }),
            )))
        }

        Some("equip_cmd") => {
            let idx = payload
                .get("equipIdx")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let cmd = payload.get("cmd").and_then(|v| v.as_u64()).unwrap_or(0);
            println!("[gauge]   → equip_cmd: idx={idx}, cmd={cmd}");
            Some(WireMsg::Ack(AckPayload::ok(
                id.into(),
                json!({
                    "entries": mock_equipment_entries(),
                    "toggled": idx,
                }),
            )))
        }

        Some("slow_command") => {
            println!("[gauge]   → Simulating slow command (not responding — will timeout)");
            None // intentionally no ack
        }

        Some(unknown) => {
            println!("[gauge]   → Unknown command '{unknown}', sending error");
            Some(WireMsg::Ack(AckPayload::err(
                id.into(),
                format!("UNKNOWN_COMMAND: {unknown}"),
            )))
        }

        None => {
            println!("[gauge]   → Unnamed command, echoing payload");
            Some(WireMsg::Ack(AckPayload::ok(id.into(), payload.clone())))
        }
    }
}

/// Mock equipment entries (matches the DC-10 equipment system).
fn mock_equipment_entries() -> Vec<Value> {
    vec![
        json!({"idx": 0, "category": "Navigation Systems", "label": "AINS-70", "available": true, "active": true}),
        json!({"idx": 1, "category": "Navigation Systems", "label": "CIV-A", "available": true, "active": false}),
        json!({"idx": 2, "category": "Navigation Systems", "label": "INS-61B", "available": true, "active": false}),
        json!({"idx": 7, "category": "Fuel Gauges", "label": "Digital", "available": true, "active": false}),
        json!({"idx": 8, "category": "Fuel Gauges", "label": "Analog", "available": true, "active": true}),
        json!({"idx": 9, "category": "Range Options", "label": "Normal", "available": true, "active": true}),
        json!({"idx": 10, "category": "Range Options", "label": "Extended Range", "available": true, "active": false}),
        json!({"idx": 40, "category": "Engine Displays", "label": "Tapes", "available": true, "active": false}),
        json!({"idx": 41, "category": "Engine Displays", "label": "Dials", "available": true, "active": true}),
        json!({"idx": 42, "category": "Engine Displays", "label": "Digital", "available": false, "active": false}),
    ]
}

fn epoch_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
