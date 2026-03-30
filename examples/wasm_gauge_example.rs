//! # Example: WASM Gauge (cannot compile without MSFS SDK)
//!
//! This file shows the complete pattern for integrating msfs-bridge-wasm
//! into a real MSFS WASM gauge. It includes:
//!
//! 1. The `CommBusBackend` implementation bridging to your `msfs` crate
//! 2. A `Router` with command handlers and event handlers
//! 3. Integration with the MSFS gauge lifecycle (`System` trait)
//! 4. Fire-and-forget events from WASM to host
//!
//! ## File layout in your addon
//!
//! ```
//! my-addon-wasm/
//! ├── Cargo.toml              # depends on msfs, msfs-bridge-wasm
//! └── src/
//!     ├── lib.rs              # export_system!
//!     ├── comm_bus_backend.rs  # CommBusBackend impl (this file's top section)
//!     ├── bridge_setup.rs      # Router + Bridge setup (this file's bottom section)
//!     └── systems/             # your sim systems
//! ```

// ═══════════════════════════════════════════════════════════════════════
// PART 1: CommBusBackend implementation
// ═══════════════════════════════════════════════════════════════════════
//
// This bridges msfs-bridge-wasm to your specific msfs crate version.
// You write this once and never touch it again.

/*
use msfs::comm_bus::{self, BroadcastFlags, Subscription};
use msfs_bridge_wasm::CommBusBackend;

/// CommBus backend wired to the `msfs` crate.
pub struct MsfsCommBus;

impl CommBusBackend for MsfsCommBus {
    type Error = comm_bus::CommBusError;
    type Subscription = Subscription;

    fn subscribe(
        event: &str,
        callback: impl Fn(&str) + 'static,
    ) -> Result<Self::Subscription, Self::Error> {
        // The msfs crate's subscribe gives &[u8], but we need &str.
        Subscription::subscribe(event, move |bytes| {
            if let Ok(s) = std::str::from_utf8(bytes) {
                callback(s);
            }
        })
    }

    fn call(event: &str, data: &str) -> Result<(), Self::Error> {
        comm_bus::call(event, data.as_bytes(), BroadcastFlags::ALL)
    }
}
*/

// ═══════════════════════════════════════════════════════════════════════
// PART 2: Bridge setup with Router
// ═══════════════════════════════════════════════════════════════════════
//
// This is your gauge's bridge integration. The Router maps command names
// to handlers, and the Bridge manages the CommBus subscription.

/*
use msfs::prelude::*;
use msfs_bridge_wasm::{Bridge, BridgeConfig, Router};
use serde_json::{json, Value};
use std::cell::RefCell;
use std::rc::Rc;

// Your simulation state (whatever your gauge manages)
struct SimState {
    temperature: f64,
    phase: String,
    equipment: Vec<EquipmentEntry>,
}

#[derive(Clone)]
struct EquipmentEntry {
    idx: u8,
    label: String,
    available: bool,
    active: bool,
}

impl SimState {
    fn new() -> Self {
        Self {
            temperature: 15.0,
            phase: "preflight".into(),
            equipment: vec![
                EquipmentEntry { idx: 0, label: "AINS-70".into(), available: true, active: true },
                EquipmentEntry { idx: 1, label: "CIV-A".into(), available: true, active: false },
            ],
        }
    }

    fn equipment_json(&self) -> Value {
        let entries: Vec<Value> = self.equipment.iter().map(|e| {
            json!({
                "idx": e.idx,
                "label": e.label,
                "available": e.available,
                "active": e.active,
            })
        }).collect();
        json!({ "entries": entries })
    }

    fn toggle_equipment(&mut self, idx: u8) -> Result<(), String> {
        let entry = self.equipment.iter_mut()
            .find(|e| e.idx == idx)
            .ok_or_else(|| format!("equipment idx {idx} not found"))?;

        if !entry.available {
            return Err(format!("equipment '{}' is not available", entry.label));
        }

        entry.active = !entry.active;
        Ok(())
    }

    fn apply_config(&mut self, data: &Value) {
        // Apply configuration from host (e.g. livery changes, settings)
        if let Some(phase) = data.get("phase").and_then(|v| v.as_str()) {
            self.phase = phase.to_string();
        }
    }
}

// The actual gauge
pub struct MyGauge {
    state: Rc<RefCell<SimState>>,
    bridge: Bridge<MsfsCommBus>,
}

impl MyGauge {
    pub fn new() -> Self {
        let state = Rc::new(RefCell::new(SimState::new()));

        // Build the router
        let router = Router::new()
            // Query equipment state
            .command("equip_query", {
                let state = Rc::clone(&state);
                move |_payload: &Value| {
                    let s = state.borrow();
                    Ok(s.equipment_json())
                }
            })
            // Toggle/enable/disable equipment
            .command("equip_cmd", {
                let state = Rc::clone(&state);
                move |payload: &Value| {
                    let idx = payload.get("equipIdx")
                        .and_then(|v| v.as_u64())
                        .ok_or("missing equipIdx")? as u8;

                    let mut s = state.borrow_mut();
                    s.toggle_equipment(idx)
                        .map_err(|e| format!("CMD_FAILED: {e}"))?;

                    Ok(s.equipment_json())
                }
            })
            // Query simulation state
            .command("get_state", {
                let state = Rc::clone(&state);
                move |_payload: &Value| {
                    let s = state.borrow();
                    Ok(json!({
                        "temperature": s.temperature,
                        "phase": s.phase,
                    }))
                }
            })
            // Handle fire-and-forget events from host
            .event("config_updated", {
                let state = Rc::clone(&state);
                move |data: &Value| {
                    let mut s = state.borrow_mut();
                    s.apply_config(data);
                    println!("[MyGauge] Config updated from host: {data}");
                }
            })
            // Fallback for unknown commands
            .fallback(|name, _payload| {
                Err(format!("UNKNOWN_COMMAND: {}", name.unwrap_or("<unnamed>")))
            });

        // Create the bridge
        //
        // The event names must match what the TS relay is configured with.
        // Convention: "youraddon/bridge_call" and "youraddon/bridge_resp"
        let bridge = Bridge::<MsfsCommBus>::new(
            BridgeConfig::new("myaddon/bridge_call", "myaddon/bridge_resp"),
            router,
        ).expect("Failed to create bridge");

        Self { state, bridge }
    }
}

impl System for MyGauge {
    fn init(&mut self, _ctx: &Context, _install: &SystemInstall) -> bool {
        println!("[MyGauge] Initialized with bridge");
        true
    }

    fn update(&mut self, _ctx: &Context, _dt: f32) -> bool {
        // Example: emit a telemetry event every frame (you'd throttle this)
        let s = self.state.borrow();
        let _ = self.bridge.emit("telemetry", json!({
            "temperature": s.temperature,
            "phase": s.phase,
        }));
        true
    }

    fn kill(&mut self, _ctx: &Context) -> bool {
        println!("[MyGauge] Killed");
        true
    }
}

msfs::export_system!(
    name = my_gauge_system,
    state = MyGauge,
    ctor = MyGauge::new()
);
*/

fn main() {
    println!("This file is documentation-only.");
    println!("See the comments for the full WASM gauge integration pattern.");
    println!();
    println!("For a runnable end-to-end demo, use:");
    println!("  cargo run --bin host-app    (terminal 1)");
    println!("  cargo run --bin fake-gauge  (terminal 2)");
}
