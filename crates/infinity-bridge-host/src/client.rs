use infinity_bridge_wire::HelloPayload;
use tokio::sync::mpsc;
use tokio::time::Instant;

pub(crate) struct Client {
    pub tx: mpsc::UnboundedSender<String>,
    pub hello: Option<HelloPayload>,
    pub last_seen: Instant,
    /// Set when the client reports [`infinity_bridge_wire::READY_EVENT`] —
    /// its own downstream link (CommBus → WASM) is bound, so a command sent
    /// here can actually be served. Relays that never send the event stay
    /// `false`, which means "unknown", not "unreachable".
    pub ready: bool,
}
