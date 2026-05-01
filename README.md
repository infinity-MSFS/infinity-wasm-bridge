# infinity-wasm-bridge

A three-layer bridge that lets an MSFS WASM gauge send and receive data to an external host application over WebSocket. WASM gauges run in a sandbox and cannot open TCP sockets directly — this library routes messages through the adjacent Coherent HTML gauge engine via the CommBus IPC mechanism.

## Performance

This bridge is not the bottleneck.

- ~20–40 ms latency floor (hard limit of MSFS WASM tick rate)
- Fully saturates WASM throughput
- ~25,000 events/sec (burst)
- ~12–13 MB/s sustained
- Zero drops under load
- Linear scaling with concurrency
- Flat latency across large payloads (100s KB+) with binary transport

### Reality

The only thing limiting performance is MSFS itself.

- ~20 ms ≈ 1 WASM tick  
- ~40 ms ≈ round trip  
- Same limitation applies to SimConnect in WASM

### Binary Transport

- No JSON on the hot path  
- No reserialization overhead  
- Stable latency regardless of payload size  
- Large payloads without degradation  

### Bottom Line

infinity-wasm-bridge fully saturates MSFS WASM performance while delivering larger payloads with a modern, seamless API when compared to SimConnect.

You are not limited by transport. You are limited by the sim.


```
 ┌─────────────────────────────── MSFS Process ──────────────────────────────────┐
 │                                                                                │
 │  ┌─────────────────────┐    CommBus JSON    ┌──────────────────────────────┐  │
 │  │  infinity-bridge-wasm   │ ◄────────────────► │  infinity-bridge-relay (JS/TS)   │  │
 │  │  (Rust WASM gauge)  │                    │  (Coherent HTML gauge)       │  │
 │  └─────────────────────┘                    └──────────┬───────────────────┘  │
 │                                                        │ WebSocket             │
 └────────────────────────────────────────────────────────┼───────────────────────┘
                                                          │
                                              ┌───────────▼───────────────────┐
                                              │  infinity-bridge-host (Rust/Tokio) │
                                              │  WebSocket server (axum)       │
                                              └───────────────────────────────┘
```

## Crates

| Crate | Description |
|---|---|
| [`infinity-bridge-wire`](crates/infinity-bridge-wire) | Shared wire format types (`no_std` compatible) |
| [`infinity-bridge-wasm`](crates/infinity-bridge-wasm) | WASM gauge side — CommBus abstraction and command router |
| [`infinity-bridge-host`](crates/infinity-bridge-host) | Host application side — async WebSocket server |
| [`infinity-bridge-relay`](ts/infinity-bridge-relay) | TypeScript relay running inside the Coherent HTML gauge |

---

## Quick Start

### 1. Host Application

Add the dependency:

```toml
[dependencies]
infinity-bridge-host = "0.1.0"
tokio = { version = "1", features = ["full"] }
```

Start the server and interact with the gauge:

```rust
use infinity_bridge_host::{BridgeServer, ServerConfig};
use serde_json::json;
use std::time::Duration;

#[tokio::main]
async fn main() {
    let config = ServerConfig::new("127.0.0.1:9876", "/bridge");
    let server = BridgeServer::start(config).await.unwrap();

    // Wait for the gauge to connect
    server.wait_connected().await;

    // Send a command and await the response
    let response = server
        .command("equip_query", json!({}), Duration::from_secs(3))
        .await
        .unwrap();
    println!("{response:#}");

    // Subscribe to events sent from the WASM gauge
    let mut events = server.subscribe_events();
    tokio::spawn(async move {
        while let Ok(event) = events.recv().await {
            println!("gauge event: {} — {:?}", event.name, event.data);
        }
    });

    // Fire a one-way event toward the gauge
    server.emit("config_updated", json!({"livery": "American Airlines"})).unwrap();
}
```

### 2. TypeScript Relay (Coherent HTML Gauge)

Install the relay package into your HTML gauge project, then initialize it in your `BaseInstrument`:

```typescript
import { BridgeRelay } from 'infinity-bridge-relay';

export class MyGauge extends BaseInstrument {
    private relay!: BridgeRelay;

    connectedCallback(): void {
        super.connectedCallback();
        this.relay = new BridgeRelay({
            wsUrl: "ws://127.0.0.1:9876/bridge",
            callEvent: "myaddon/bridge_call",
            responseEvent: "myaddon/bridge_resp",
            hello: { client: "msfs-gauge", aircraft: "DC-10-30" },
        });
        this.relay.init();
    }

    Update(): void {
        super.Update();
        this.relay.update();
    }
}
```

### 3. WASM Gauge (Rust)

The WASM crate has no dependency on the MSFS SDK — you provide a thin `CommBusBackend` impl:

```rust
use infinity_bridge_wasm::{CommBusBackend, BridgeConfig, Bridge, Router};

// Wire up your msfs crate version
struct MyBackend;

impl CommBusBackend for MyBackend {
    type Error = msfs::MSFSError;
    type Subscription = msfs::sys::CommBusSubscription;

    fn subscribe(event: &str, cb: impl Fn(&str) + 'static) -> Result<Self::Subscription, Self::Error> {
        msfs::commbus::CommBus::subscribe(event, cb)
    }
    fn call(event: &str, data: &str) -> Result<(), Self::Error> {
        msfs::commbus::CommBus::call(event, data)
    }
}

// Set up the router — event names must match the relay config
let config = BridgeConfig::new("myaddon/bridge_call", "myaddon/bridge_resp");

let router = Router::new()
    .command("equip_query", move |_| {
        Ok(json!({"entries": ["item_a", "item_b"]}))
    })
    .command("set_config", move |payload| {
        // apply payload to state...
        Ok(json!({"ok": true}))
    })
    .event("config_updated", move |data| {
        println!("config changed: {data:?}");
    });

let bridge = Bridge::<MyBackend>::new(config, router)?;

// Later, emit a fire-and-forget event to the host
bridge.emit("telemetry", json!({"altitude_ft": 35_000, "speed_kts": 285}))?;
```

---

## Architecture

### Wire Protocol

All messages are JSON with a `"t"` discriminant field. Defined in `infinity-bridge-wire` and mirrored in the TypeScript relay.

| Message | Direction | Description |
|---|---|---|
| `hello` | Gauge → Host | First message after WebSocket connect — identifies the client |
| `ping` | Host → Gauge | Keepalive probe |
| `pong` | Gauge → Host | Keepalive response |
| `cmd` | Host → Gauge | RPC request with a correlation UUID |
| `ack` | Gauge → Host | RPC response (success or error) |
| `event` | Either direction | Fire-and-forget named event |

```json
{"t":"hello","client":"msfs-gauge","aircraft":"DC-10-30","tail":"N1819U","session":"1234567","v":1}
{"t":"ping","ts":1711234567890}
{"t":"pong","ts":1711234567890}
{"t":"cmd","id":"550e8400-e29b-41d4-a716-446655440000","name":"equip_query","payload":{}}
{"t":"ack","id":"550e8400-e29b-41d4-a716-446655440000","ok":true,"response":{"entries":[]}}
{"t":"ack","id":"...","ok":false,"error":"equipment idx 5 not found"}
{"t":"event","name":"telemetry","data":{"altitude_ft":35000,"speed_kts":285}}
```

### Data Flow: Host → WASM (command/RPC)

```
BridgeServer::command("equip_query", payload, timeout)
  → UUID assigned, oneshot registered in pending map
  → WireMsg::Cmd serialized → WebSocket → JS relay
      → dedup check (duplicate IDs replied to immediately)
      → CommBus.call(callEvent, {requestId, payload})
          → Bridge<B>::dispatch (sync WASM callback)
              → Router matches handler, returns Result<Value>
              → B::call(responseEvent, {requestId, ok, response})
          → JS receives CommBus response
          → WireMsg::Ack sent over WebSocket
  → Hub resolves oneshot → caller receives Ok(Value) or Err
```

### Data Flow: WASM → Host (fire-and-forget event)

```
bridge.emit("telemetry", json!({...}))
  → WireMsg::Event serialized
  → B::call(responseEvent, json)       // direct CommBus, no requestId
      → JS relay: t === "event" → forwards raw over WebSocket
          → Hub.dispatch_event
              → event_tx.send(EventPayload)
                  → all broadcast::Receiver<EventPayload> subscribers notified
```

### Deduplication

The TypeScript relay maintains a bounded LRU ring (`DedupRing`, default capacity 128) keyed on command IDs. If a command is retransmitted before an ack is sent (e.g. during reconnect), the relay responds immediately with `{ok: true, duplicate: true}` without forwarding to the WASM side.

### Reconnection

The relay reconnects to the WebSocket server automatically using exponential backoff with jitter: `min(maxReconnectMs, baseReconnectMs × 2^attempt) + rand(0..250ms)`. Defaults: base 250 ms, max 30 s.

---

## Configuration Reference

### `ServerConfig` (host)

```rust
ServerConfig::new("127.0.0.1:9876", "/bridge")
    .ping_interval(Duration::from_secs(10))
    .ping_timeout(Duration::from_secs(30))
    .event_channel_capacity(256)
```

### `BridgeRelayConfig` (TypeScript)

```typescript
{
    wsUrl: string;            // WebSocket URL of the host server
    callEvent: string;        // CommBus event name used to call into WASM
    responseEvent: string;    // CommBus event name WASM responds on
    hello?: {                 // Optional handshake metadata
        client?: string;
        aircraft?: string;
        tail?: string;
        session?: string;
        meta?: unknown;
    };
    dedupCapacity?: number;   // Default 128
    baseReconnectMs?: number; // Default 250
    maxReconnectMs?: number;  // Default 30_000
    protocolVersion?: number; // Default 1
}
```

### `BridgeConfig` (WASM)

```rust
BridgeConfig::new("myaddon/bridge_call", "myaddon/bridge_resp")
```

The CommBus event names must be consistent across all three layers.

---

## Integrating with Tauri

`BridgeServer` can be mounted inside an existing axum router, making it straightforward to embed inside a Tauri app:

```rust
let (bridge, bridge_router) = BridgeServer::router(config);
let app = axum::Router::new()
    .merge(bridge_router)
    .route("/api/health", get(|| async { "ok" }));

// Forward gauge events to the Tauri webview
let mut events = bridge.subscribe_events();
let handle = app_handle.clone();
tokio::spawn(async move {
    while let Ok(event) = events.recv().await {
        handle.emit(&event.name, &event.data).ok();
    }
});
```

---

## Examples

| File | Description |
|---|---|
| [examples/host-app/src/main.rs](examples/host-app/src/main.rs) | Full host-side demonstration — commands, events, and error handling |
| [examples/host-app/src/fake_gauge.rs](examples/host-app/src/fake_gauge.rs) | Simulates the relay + WASM pipeline as a plain WebSocket client for testing |
| [examples/wasm_gauge_example.rs](examples/wasm_gauge_example.rs) | Complete WASM integration reference (requires MSFS SDK at build time) |
| [examples/cohierent.ts](examples/cohierent.ts) | TypeScript relay usage inside a `BaseInstrument` |

Run the bundled host example (starts server + fake gauge in the same process):

```sh
cargo run --example host-app
```
