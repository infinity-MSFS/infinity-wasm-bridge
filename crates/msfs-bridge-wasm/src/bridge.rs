use std::cell::RefCell;
use std::marker::PhantomData;
use std::rc::Rc;

use msfs_bridge_wire::{BridgeError, EventPayload, WireMsg};
use serde_json::Value;

use crate::backend::CommBusBackend;

pub struct BridgeConfig {
    pub call_event: String,
    pub response_event: String,
}

impl BridgeConfig {
    pub fn new(call_event: impl Into<String>, response_event: impl Into<String>) -> Self {
        Self {
            call_event: call_event.into(),
            response_event: response_event.into(),
        }
    }
}

pub trait BridgeHandler: 'static {
    /// Handle a command (request/response). Must return a result.
    ///
    /// `name` is the optional command name from the wire message.
    /// `payload` is the JSON payload.
    fn on_command(&self, name: Option<&str>, payload: &Value) -> Result<Value, String>;

    /// Handle a fire-and-forget event from the host.
    ///
    /// Default implementation does nothing. Override to process events.
    fn on_event(&self, _name: &str, _data: &Value) {}
}

impl<F> BridgeHandler for F
where
    F: Fn(Option<&str>, &Value) -> Result<Value, String> + 'static,
{
    fn on_command(&self, name: Option<&str>, payload: &Value) -> Result<Value, String> {
        (self)(name, payload)
    }
}

type CommandFn = Box<dyn Fn(&Value) -> Result<Value, String>>;
type EventFn = Box<dyn Fn(&Value)>;

/// Declarative command router for the WASM bridge.
///
/// Maps command names to handlers, with an optional fallback for unnamed
/// or unrecognized commands.
///
/// ```rust,ignore
/// use msfs_bridge_wasm::Router;
/// use serde_json::{json, Value};
/// use std::cell::RefCell;
///
/// let state = RefCell::new(MyState::new());
///
/// let router = Router::new()
///     .command("get_state", {
///         let state = state.clone();
///         move |_payload: &Value| {
///             let s = state.borrow();
///             Ok(json!({ "temp": s.temperature }))
///         }
///     })
///     .command("set_config", {
///         let state = state.clone();
///         move |payload: &Value| {
///             let mut s = state.borrow_mut();
///             s.apply_config(payload);
///             Ok(json!({"ok": true}))
///         }
///     })
///     .event("config_updated", {
///         let state = state.clone();
///         move |data: &Value| {
///             let mut s = state.borrow_mut();
///             s.apply_config(data);
///         }
///     })
///     .fallback(|name, payload| {
///         Err(format!("unknown command: {}", name.unwrap_or("<unnamed>")))
///     });
/// ```
pub struct Router {
    commands: Vec<(&'static str, CommandFn)>,
    events: Vec<(&'static str, EventFn)>,
    fallback: Option<Box<dyn Fn(Option<&str>, &Value) -> Result<Value, String>>>,
}

impl Router {
    pub fn new() -> Self {
        Self {
            commands: Vec::new(),
            events: Vec::new(),
            fallback: None,
        }
    }

    pub fn command(
        mut self,
        name: &'static str,
        handler: impl Fn(&Value) -> Result<Value, String> + 'static,
    ) -> Self {
        self.commands.push((name, Box::new(handler)));
        self
    }

    pub fn event(mut self, name: &'static str, handler: impl Fn(&Value) + 'static) -> Self {
        self.events.push((name, Box::new(handler)));
        self
    }

    pub fn fallback(
        mut self,
        handler: impl Fn(Option<&str>, &Value) -> Result<Value, String> + 'static,
    ) -> Self {
        self.fallback = Some(Box::new(handler));
        self
    }
}

impl BridgeHandler for Router {
    fn on_command(&self, name: Option<&str>, payload: &Value) -> Result<Value, String> {
        if let Some(name) = name {
            for (cmd_name, handler) in &self.commands {
                if *cmd_name == name {
                    return handler(payload);
                }
            }
        }
        if let Some(ref fallback) = self.fallback {
            return fallback(name, payload);
        }

        Err(format!("UNKNOWN_COMMAND: {}", name.unwrap_or("<unnamed>")))
    }

    fn on_event(&self, name: &str, data: &Value) {
        for (evt_name, handler) in &self.events {
            if *evt_name == name {
                handler(data);
                return;
            }
        }
    }
}

#[derive(serde::Deserialize)]
struct CommBusEnvelope {
    #[serde(rename = "requestId")]
    request_id: String,
    payload: Value,
}

#[derive(serde::Serialize)]
struct CommBusResponse {
    #[serde(rename = "requestId")]
    request_id: String,
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    response: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

struct BridgeShared {
    response_event: String,
}

pub struct Bridge<B: CommBusBackend> {
    _subscription: B::Subscription,
    shared: Rc<RefCell<BridgeShared>>,
    _marker: PhantomData<B>,
}

impl<B: CommBusBackend> Bridge<B> {
    pub fn new(config: BridgeConfig, handler: impl BridgeHandler) -> Result<Self, BridgeError> {
        let shared = Rc::new(RefCell::new(BridgeShared {
            response_event: config.response_event.clone(),
        }));

        let shared_for_cb = Rc::clone(&shared);

        let subscription = B::subscribe(&config.call_event, move |raw| {
            Self::dispatch(&shared_for_cb, &handler, raw);
        })
        .map_err(|e| BridgeError::transport(format!("CommBus subscribe failed: {e}")))?;

        Ok(Self {
            _subscription: subscription,
            shared,
            _marker: PhantomData,
        })
    }

    fn dispatch(shared: &Rc<RefCell<BridgeShared>>, handler: &impl BridgeHandler, raw: &str) {
        let envelope: CommBusEnvelope = match serde_json::from_str(raw) {
            Ok(e) => e,
            Err(e) => {
                eprintln!("[msfs-bridge] Failed to parse CommBus envelope: {e}");
                return;
            }
        };

        let payload_str = match serde_json::to_string(&envelope.payload) {
            Ok(s) => s,
            Err(_) => {
                Self::send_response(
                    shared,
                    &envelope.request_id,
                    Err("internal: re-serialize failed".into()),
                );
                return;
            }
        };

        match WireMsg::from_json(&payload_str) {
            Ok(WireMsg::Cmd(cmd)) => {
                let result = handler.on_command(cmd.name.as_deref(), &cmd.payload);
                Self::send_response(shared, &envelope.request_id, result);
            }
            Ok(WireMsg::Event(evt)) => {
                handler.on_event(&evt.name, &evt.data);
            }
            _ => {
                let result = handler.on_command(None, &envelope.payload);
                Self::send_response(shared, &envelope.request_id, result);
            }
        }
    }

    fn send_response(
        shared: &Rc<RefCell<BridgeShared>>,
        request_id: &str,
        result: Result<Value, String>,
    ) {
        let response = CommBusResponse {
            request_id: request_id.to_string(),
            ok: result.is_ok(),
            response: result.as_ref().ok().cloned(),
            error: result.err(),
        };

        let resp_json = match serde_json::to_string(&response) {
            Ok(j) => j,
            Err(e) => {
                eprintln!("[msfs-bridge] Failed to serialize response: {e}");
                return;
            }
        };

        let event_name = shared.borrow().response_event.clone();

        if let Err(e) = B::call(&event_name, &resp_json) {
            eprintln!("[msfs-bridge] CommBus send failed: {e}");
        }
    }

    pub fn emit(&self, name: impl Into<String>, data: Value) -> Result<(), BridgeError> {
        let msg = WireMsg::Event(EventPayload::new(name, data));
        let json = msg.to_json()?;

        let event_name = self.shared.borrow().response_event.clone();

        B::call(&event_name, &json)
            .map_err(|e| BridgeError::transport(format!("CommBus send failed: {e}")))
    }
}
