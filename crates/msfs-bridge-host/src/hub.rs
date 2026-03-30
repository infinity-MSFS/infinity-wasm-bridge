use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use msfs_bridge_wire::{AckPayload, BridgeError, CmdPayload, EventPayload, HelloPayload, WireMsg};
use serde_json::Value;
use tokio::sync::{Mutex, broadcast, mpsc, oneshot, watch};
use uuid::Uuid;

use crate::client::Client;

#[derive(Debug, Clone)]
pub struct ClientInfo {
    pub id: u64,
    pub hello: Option<HelloPayload>,
}

pub(crate) struct Hub {
    next_id: AtomicU64,
    clients: Mutex<HashMap<u64, Client>>,
    pending: Mutex<HashMap<String, oneshot::Sender<AckPayload>>>,

    event_tx: broadcast::Sender<EventPayload>,

    connection_tx: watch::Sender<bool>,
    connection_rx: watch::Receiver<bool>,

    connect_notify: tokio::sync::Notify,
}

impl Hub {
    pub fn new(event_capacity: usize) -> Arc<Self> {
        let (event_tx, _) = broadcast::channel(event_capacity);
        let (connection_tx, connection_rx) = watch::channel(false);

        Arc::new(Self {
            next_id: AtomicU64::new(1),
            clients: Mutex::new(HashMap::new()),
            pending: Mutex::new(HashMap::new()),
            event_tx,
            connection_tx,
            connection_rx,
            connect_notify: tokio::sync::Notify::new(),
        })
    }

    pub async fn register_client(&self, tx: mpsc::UnboundedSender<String>) -> u64 {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let mut clients = self.clients.lock().await;
        clients.insert(
            id,
            Client {
                tx,
                hello: None,
                last_seen: tokio::time::Instant::now(),
            },
        );
        self.connection_tx.send_replace(true);
        self.connect_notify.notify_waiters();
        id
    }

    pub async fn set_client_hello(&self, id: u64, hello: HelloPayload) {
        let mut clients = self.clients.lock().await;
        if let Some(c) = clients.get_mut(&id) {
            c.hello = Some(hello);
        }
    }

    pub async fn unregister_client(&self, id: u64) {
        let mut clients = self.clients.lock().await;
        clients.remove(&id);
        let connected = !clients.is_empty();
        self.connection_tx.send_replace(connected);
    }

    pub async fn touch_client(&self, id: u64) {
        let mut clients = self.clients.lock().await;
        if let Some(c) = clients.get_mut(&id) {
            c.last_seen = tokio::time::Instant::now();
        }
    }

    pub async fn reap_dead_clients(&self, timeout: Duration) -> Vec<u64> {
        let now = tokio::time::Instant::now();
        let mut clients = self.clients.lock().await;
        let dead: Vec<u64> = clients
            .iter()
            .filter(|(_, c)| now.duration_since(c.last_seen) > timeout)
            .map(|(&id, _)| id)
            .collect();

        for &id in &dead {
            clients.remove(&id);
        }

        if !dead.is_empty() {
            let connected = !clients.is_empty();
            self.connection_tx.send_replace(connected);
        }

        dead
    }

    pub async fn is_connected(&self) -> bool {
        !self.clients.lock().await.is_empty()
    }

    pub async fn wait_connected(&self) {
        loop {
            if self.is_connected().await {
                return;
            }
            self.connect_notify.notified().await;
        }
    }

    pub fn subscribe_connection_status(&self) -> watch::Receiver<bool> {
        self.connection_rx.clone()
    }

    pub async fn connected_clients(&self) -> Vec<ClientInfo> {
        let clients = self.clients.lock().await;
        clients
            .iter()
            .map(|(&id, c)| ClientInfo {
                id,
                hello: c.hello.clone(),
            })
            .collect()
    }

    pub fn subscribe_events(&self) -> broadcast::Receiver<EventPayload> {
        self.event_tx.subscribe()
    }

    pub fn dispatch_event(&self, event: EventPayload) {
        let _ = self.event_tx.send(event);
    }

    pub async fn emit(&self, name: impl Into<String>, data: Value) -> Result<(), BridgeError> {
        let msg = WireMsg::Event(EventPayload::new(name, data));
        let json = msg.to_json()?;

        let clients = self.clients.lock().await;
        if clients.is_empty() {
            return Err(BridgeError::no_clients(
                "no gauges connected — event dropped",
            ));
        }

        let mut send_failures = 0u32;
        for client in clients.values() {
            if client.tx.send(json.clone()).is_err() {
                send_failures += 1;
            }
        }

        if send_failures > 0 && send_failures as usize == clients.len() {
            return Err(BridgeError::transport(
                "all gauge connections failed to accept event",
            ));
        }

        Ok(())
    }

    pub async fn command(
        &self,
        name: Option<&str>,
        payload: Value,
        timeout: Duration,
    ) -> Result<Value, BridgeError> {
        let id = Uuid::new_v4().to_string();

        let cmd = match name {
            Some(n) => CmdPayload::named(id.clone(), n, payload),
            None => CmdPayload::new(id.clone(), payload),
        };
        let msg = WireMsg::Cmd(cmd);
        let json = msg.to_json()?;

        let (ack_tx, ack_rx) = oneshot::channel();
        self.pending.lock().await.insert(id.clone(), ack_tx);

        {
            let clients = self.clients.lock().await;
            if clients.is_empty() {
                self.pending.lock().await.remove(&id);
                return Err(BridgeError::no_clients(
                    "no gauges connected — cannot send command",
                ));
            }
            for client in clients.values() {
                let _ = client.tx.send(json.clone());
            }
        }

        let ack_result = tokio::time::timeout(timeout, ack_rx).await;

        self.pending.lock().await.remove(&id);

        match ack_result {
            Ok(Ok(ack)) => {
                if ack.ok {
                    Ok(ack.response.unwrap_or(Value::Null))
                } else {
                    Err(BridgeError::application(
                        ack.error.unwrap_or_else(|| "unknown error".into()),
                    ))
                }
            }
            Ok(Err(_)) => Err(BridgeError::transport(
                "all gauge connections dropped before ack",
            )),
            Err(_) => Err(BridgeError::timeout(format!(
                "no ack received within {timeout:?}"
            ))),
        }
    }

    pub async fn dispatch_ack(&self, ack: AckPayload) {
        let tx = self.pending.lock().await.remove(&ack.id);
        if let Some(tx) = tx {
            let _ = tx.send(ack);
        }
    }

    pub async fn send_to(&self, client_id: u64, json: String) -> Result<(), BridgeError> {
        let clients = self.clients.lock().await;
        let client = clients
            .get(&client_id)
            .ok_or_else(|| BridgeError::transport(format!("client {client_id} not found")))?;
        client
            .tx
            .send(json)
            .map_err(|_| BridgeError::transport(format!("client {client_id} channel closed")))
    }

    pub async fn broadcast(&self, json: String) -> Result<(), BridgeError> {
        let clients = self.clients.lock().await;
        if clients.is_empty() {
            return Err(BridgeError::no_clients("no gauges connected"));
        }
        for client in clients.values() {
            let _ = client.tx.send(json.clone());
        }
        Ok(())
    }
}
