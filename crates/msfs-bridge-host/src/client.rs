use msfs_bridge_wire::HelloPayload;
use tokio::sync::mpsc;
use tokio::time::Instant;

pub(crate) struct Client {
    pub tx: mpsc::UnboundedSender<String>,
    pub hello: Option<HelloPayload>,
    pub last_seen: Instant,
}
