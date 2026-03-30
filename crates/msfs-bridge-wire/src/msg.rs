use alloc::string::String;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "t")]
pub enum WireMsg {
    // ── Connection lifecycle ─────────────────────────────────────────
    /// Sent by the relay gauge immediately after WebSocket connect.
    #[serde(rename = "hello")]
    Hello(HelloPayload),

    /// Keepalive probe. Originated by the host, relayed to the gauge.
    #[serde(rename = "ping")]
    Ping {
        /// Millisecond timestamp (originator's clock).
        #[serde(default)]
        ts: Option<u64>,
    },

    /// Keepalive response. Sent by the relay back to the host.
    #[serde(rename = "pong")]
    Pong {
        /// Echoed timestamp from the ping.
        #[serde(default)]
        ts: Option<u64>,
    },

    // ── Request / response ───────────────────────────────────────────
    /// Command from host → WASM (routed through relay).
    /// The WASM side must reply with an [`Ack`] carrying the same `id`.
    #[serde(rename = "cmd")]
    Cmd(CmdPayload),

    /// Acknowledgement from WASM → host (routed through relay).
    #[serde(rename = "ack")]
    Ack(AckPayload),

    // ── Fire-and-forget ──────────────────────────────────────────────
    /// Unacknowledged event in either direction.
    #[serde(rename = "event")]
    Event(EventPayload),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HelloPayload {
    /// Arbitrary client identifier (e.g. `"msfs-gauge"`).
    #[serde(default)]
    pub client: Option<String>,

    /// Aircraft type (e.g. `"DC-10"`).
    #[serde(default)]
    pub aircraft: Option<String>,

    /// Tail number or livery identifier.
    #[serde(default)]
    pub tail: Option<String>,

    /// Session identifier — unique per flight session.
    #[serde(default)]
    pub session: Option<String>,

    /// Protocol version spoken by this client.
    #[serde(default)]
    pub v: Option<u32>,

    /// Arbitrary extra metadata the consumer wants to pass on connect.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CmdPayload {
    /// Correlation ID — the ack must carry the same value.
    pub id: String,

    /// Optional command name for routing on the WASM side.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,

    /// Arbitrary JSON payload.
    pub payload: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AckPayload {
    /// Correlation ID matching the originating [`CmdPayload::id`].
    pub id: String,

    /// `true` if the command succeeded.
    pub ok: bool,

    /// Error description when `ok == false`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,

    /// Response data when `ok == true`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response: Option<Value>,

    /// Set to `true` if this is a duplicate ack (idempotency hit).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duplicate: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventPayload {
    /// Event name for routing (e.g. `"state_changed"`).
    pub name: String,

    /// Arbitrary JSON data.
    pub data: Value,
}

impl AckPayload {
    pub fn ok(id: String, response: Value) -> Self {
        Self {
            id,
            ok: true,
            error: None,
            response: Some(response),
            duplicate: None,
        }
    }

    pub fn err(id: String, error: String) -> Self {
        Self {
            id,
            ok: false,
            error: Some(error),
            response: None,
            duplicate: None,
        }
    }

    pub fn duplicate(id: String) -> Self {
        Self {
            id,
            ok: true,
            error: None,
            response: None,
            duplicate: Some(true),
        }
    }
}

impl CmdPayload {
    pub fn new(id: String, payload: Value) -> Self {
        Self {
            id,
            name: None,
            payload,
        }
    }

    pub fn named(id: String, name: impl Into<String>, payload: Value) -> Self {
        Self {
            id,
            name: Some(name.into()),
            payload,
        }
    }
}

impl EventPayload {
    pub fn new(name: impl Into<String>, data: Value) -> Self {
        Self {
            name: name.into(),
            data,
        }
    }
}

impl WireMsg {
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }

    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn round_trip_cmd() {
        let cmd = WireMsg::Cmd(CmdPayload::named(
            "abc-123".into(),
            "get_state",
            json!({"key": "value"}),
        ));
        let json = cmd.to_json().unwrap();
        let parsed = WireMsg::from_json(&json).unwrap();

        match parsed {
            WireMsg::Cmd(c) => {
                assert_eq!(c.id, "abc-123");
                assert_eq!(c.name.as_deref(), Some("get_state"));
                assert_eq!(c.payload, json!({"key": "value"}));
            }
            other => panic!("expected Cmd, got {:?}", other),
        }
    }

    #[test]
    fn round_trip_ack_ok() {
        let ack = WireMsg::Ack(AckPayload::ok("abc-123".into(), json!(42)));
        let json = ack.to_json().unwrap();
        let parsed = WireMsg::from_json(&json).unwrap();

        match parsed {
            WireMsg::Ack(a) => {
                assert!(a.ok);
                assert_eq!(a.response, Some(json!(42)));
                assert!(a.error.is_none());
            }
            other => panic!("expected Ack, got {:?}", other),
        }
    }

    #[test]
    fn round_trip_ack_err() {
        let ack = WireMsg::Ack(AckPayload::err("abc-123".into(), "boom".into()));
        let json = ack.to_json().unwrap();
        let parsed = WireMsg::from_json(&json).unwrap();

        match parsed {
            WireMsg::Ack(a) => {
                assert!(!a.ok);
                assert_eq!(a.error.as_deref(), Some("boom"));
            }
            other => panic!("expected Ack, got {:?}", other),
        }
    }

    #[test]
    fn round_trip_event() {
        let evt = WireMsg::Event(EventPayload::new(
            "state_changed",
            json!({"phase": "cruise"}),
        ));
        let json = evt.to_json().unwrap();
        let parsed = WireMsg::from_json(&json).unwrap();

        match parsed {
            WireMsg::Event(e) => {
                assert_eq!(e.name, "state_changed");
                assert_eq!(e.data, json!({"phase": "cruise"}));
            }
            other => panic!("expected Event, got {:?}", other),
        }
    }

    #[test]
    fn round_trip_hello() {
        let hello = WireMsg::Hello(HelloPayload {
            client: Some("msfs-gauge".into()),
            aircraft: Some("DC-10".into()),
            tail: None,
            session: Some("12345".into()),
            v: Some(1),
            meta: None,
        });
        let json = hello.to_json().unwrap();
        let parsed = WireMsg::from_json(&json).unwrap();

        match parsed {
            WireMsg::Hello(h) => {
                assert_eq!(h.client.as_deref(), Some("msfs-gauge"));
                assert_eq!(h.v, Some(1));
            }
            other => panic!("expected Hello, got {:?}", other),
        }
    }

    #[test]
    fn round_trip_ping_pong() {
        let ping = WireMsg::Ping {
            ts: Some(1234567890),
        };
        let json = ping.to_json().unwrap();
        assert!(json.contains("\"t\":\"ping\""));

        let pong = WireMsg::Pong {
            ts: Some(1234567890),
        };
        let json = pong.to_json().unwrap();
        let parsed = WireMsg::from_json(&json).unwrap();
        match parsed {
            WireMsg::Pong { ts } => assert_eq!(ts, Some(1234567890)),
            other => panic!("expected Pong, got {:?}", other),
        }
    }

    #[test]
    fn duplicate_ack() {
        let ack = WireMsg::Ack(AckPayload::duplicate("abc-123".into()));
        let json = ack.to_json().unwrap();
        let parsed = WireMsg::from_json(&json).unwrap();

        match parsed {
            WireMsg::Ack(a) => {
                assert!(a.ok);
                assert_eq!(a.duplicate, Some(true));
            }
            other => panic!("expected Ack, got {:?}", other),
        }
    }

    #[test]
    fn unknown_type_fails() {
        let bad = r#"{"t":"banana","stuff":42}"#;
        assert!(WireMsg::from_json(bad).is_err());
    }

    #[test]
    fn skip_serializing_none_fields() {
        let ack = AckPayload::ok("id".into(), json!(null));
        let json = serde_json::to_string(&ack).unwrap();
        // `error` and `duplicate` should be absent, not null
        assert!(!json.contains("\"error\""));
        assert!(!json.contains("\"duplicate\""));
    }
}
