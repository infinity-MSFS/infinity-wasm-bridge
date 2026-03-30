use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::IntoResponse;
use axum::routing::get;
use futures::{SinkExt, StreamExt};
use msfs_bridge_wire::{BridgeError, EventPayload, WireMsg};
use serde_json::Value;
use tokio::sync::{broadcast, mpsc, watch};

use crate::hub::{ClientInfo, Hub};

pub struct ServerConfig {
    pub bind_addr: String,
    pub ws_path: String,
    pub event_capacity: usize,
    pub ping_interval: Duration,
    pub ping_timeout: Duration,
}

impl ServerConfig {
    pub fn new(bind_addr: impl Into<String>, ws_path: impl Into<String>) -> Self {
        Self {
            bind_addr: bind_addr.into(),
            ws_path: ws_path.into(),
            event_capacity: 256,
            ping_interval: Duration::from_secs(5),
            ping_timeout: Duration::from_secs(15),
        }
    }
}

#[derive(Clone)]
pub struct BridgeServer {
    hub: Arc<Hub>,
    config: Arc<ServerConfig>,
}

impl BridgeServer {
    /// Start the bridge server and begin accepting connections.
    ///
    /// This spawns a tokio task running the axum HTTP server.
    /// The returned `BridgeServer` handle can be used to interact with
    /// connected gauges.
    pub async fn start(config: ServerConfig) -> Result<Self, BridgeError> {
        let hub = Hub::new(config.event_capacity);
        let config = Arc::new(config);

        let server = Self {
            hub: Arc::clone(&hub),
            config: Arc::clone(&config),
        };

        let app_state = AppState {
            hub: Arc::clone(&hub),
            config: Arc::clone(&config),
        };

        let app = Router::new()
            .route(&config.ws_path, get(ws_upgrade))
            .route("/health", get(health))
            .with_state(app_state);

        let listener = tokio::net::TcpListener::bind(&config.bind_addr)
            .await
            .map_err(|e| BridgeError::transport(format!("bind failed: {e}")))?;

        tokio::spawn(async move {
            if let Err(e) = axum::serve(listener, app).await {
                eprintln!("[msfs-bridge-host] Server error: {e}");
            }
        });

        Ok(server)
    }

    /// Build an axum [`Router`] without starting a listener.
    ///
    /// Useful when you want to mount the bridge as a nested route
    /// inside an existing axum application.
    pub fn router(config: ServerConfig) -> (Self, Router) {
        let hub = Hub::new(config.event_capacity);
        let config = Arc::new(config);

        let server = Self {
            hub: Arc::clone(&hub),
            config: Arc::clone(&config),
        };

        let app_state = AppState {
            hub: Arc::clone(&hub),
            config: Arc::clone(&config),
        };

        let router = Router::new()
            .route(&config.ws_path, get(ws_upgrade))
            .route("/health", get(health))
            .with_state(app_state);

        (server, router)
    }

    // ── Commands ─────────────────────────────────────────────────────

    /// Send a named command to all connected gauges and await the first ack.
    pub async fn command(
        &self,
        name: &str,
        payload: Value,
        timeout: Duration,
    ) -> Result<Value, BridgeError> {
        self.hub.command(Some(name), payload, timeout).await
    }

    /// Send an unnamed command (payload-only) and await the first ack.
    pub async fn command_raw(
        &self,
        payload: Value,
        timeout: Duration,
    ) -> Result<Value, BridgeError> {
        self.hub.command(None, payload, timeout).await
    }

    // ── Events ───────────────────────────────────────────────────────

    /// Send a fire-and-forget event to all connected gauges.
    ///
    /// Returns `Ok(())` if the event was dispatched to at least one gauge.
    /// Returns `Err(NoClients)` if no gauges are connected.
    pub async fn emit(&self, name: impl Into<String>, data: Value) -> Result<(), BridgeError> {
        self.hub.emit(name, data).await
    }

    /// Subscribe to events received from gauges.
    ///
    /// Returns a broadcast receiver. Events are delivered as they arrive
    /// from any connected gauge. If the receiver falls behind by
    /// `event_capacity` messages, older events are dropped.
    pub fn subscribe_events(&self) -> broadcast::Receiver<EventPayload> {
        self.hub.subscribe_events()
    }

    // ── Connection status ────────────────────────────────────────────

    /// Returns `true` if at least one gauge is connected.
    pub async fn is_connected(&self) -> bool {
        self.hub.is_connected().await
    }

    /// Wait until at least one gauge connects.
    pub async fn wait_connected(&self) {
        self.hub.wait_connected().await
    }

    /// Get a watch channel that tracks connection status.
    ///
    /// The value is `true` when at least one gauge is connected,
    /// `false` when all gauges have disconnected.
    pub fn connection_status(&self) -> watch::Receiver<bool> {
        self.hub.subscribe_connection_status()
    }

    /// List all currently connected clients.
    pub async fn clients(&self) -> Vec<ClientInfo> {
        self.hub.connected_clients().await
    }
}

#[derive(Clone)]
struct AppState {
    hub: Arc<Hub>,
    config: Arc<ServerConfig>,
}

async fn health() -> impl IntoResponse {
    "ok"
}

async fn ws_upgrade(ws: WebSocketUpgrade, State(state): State<AppState>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

async fn handle_socket(socket: WebSocket, state: AppState) {
    let (mut ws_tx, mut ws_rx) = socket.split();
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<String>();

    let client_id = state.hub.register_client(out_tx.clone()).await;

    let writer = tokio::spawn(async move {
        while let Some(text) = out_rx.recv().await {
            if ws_tx.send(Message::Text(text.into())).await.is_err() {
                break;
            }
        }
    });

    let hub_for_ping = Arc::clone(&state.hub);
    let ping_interval = state.config.ping_interval;
    let ping_timeout = state.config.ping_timeout;
    let ping_task = tokio::spawn(async move {
        let mut interval = tokio::time::interval(ping_interval);
        interval.tick().await;
        loop {
            interval.tick().await;

            {
                let dead = hub_for_ping.reap_dead_clients(ping_timeout).await;
                if dead.contains(&client_id) {
                    break;
                }
            }

            let ping = WireMsg::Ping {
                ts: Some(
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis() as u64,
                ),
            };
            let json = match ping.to_json() {
                Ok(j) => j,
                Err(_) => continue,
            };
            if hub_for_ping.send_to(client_id, json).await.is_err() {
                break;
            }
        }
    });

    while let Some(Ok(msg)) = ws_rx.next().await {
        match msg {
            Message::Text(text) => {
                state.hub.touch_client(client_id).await;

                let wire = match WireMsg::from_json(&text) {
                    Ok(w) => w,
                    Err(_) => continue,
                };

                match wire {
                    WireMsg::Hello(hello) => {
                        state.hub.set_client_hello(client_id, hello).await;
                    }
                    WireMsg::Ack(ack) => {
                        state.hub.dispatch_ack(ack).await;
                    }
                    WireMsg::Event(event) => {
                        state.hub.dispatch_event(event);
                    }
                    WireMsg::Pong { .. } => {
                        // last_seen already updated above via touch_client
                    }
                    WireMsg::Cmd(cmd) => {
                        state.hub.dispatch_event(EventPayload::new(
                            cmd.name.unwrap_or_else(|| "cmd".into()),
                            cmd.payload,
                        ));
                    }
                    WireMsg::Ping { ts } => {
                        let pong = WireMsg::Pong { ts };
                        if let Ok(json) = pong.to_json() {
                            let _ = state.hub.send_to(client_id, json).await;
                        }
                    }
                }
            }
            Message::Close(_) => break,
            _ => {}
        }
    }

    state.hub.unregister_client(client_id).await;
    ping_task.abort();
    drop(out_tx);
    let _ = writer.await;
}
