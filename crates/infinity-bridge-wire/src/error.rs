use alloc::string::String;
use core::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    Transport,
    Timeout,
    Protocol,
    Application,
    NoClients,
}

impl fmt::Display for ErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transport => f.write_str("TRANSPORT"),
            Self::Timeout => f.write_str("TIMEOUT"),
            Self::Protocol => f.write_str("PROTOCOL"),
            Self::Application => f.write_str("APPLICATION"),
            Self::NoClients => f.write_str("NO_CLIENTS"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct BridgeError {
    kind: ErrorKind,
    message: String,
}

impl BridgeError {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    pub fn transport(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Transport, message)
    }

    pub fn timeout(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Timeout, message)
    }

    pub fn protocol(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Protocol, message)
    }

    pub fn application(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Application, message)
    }

    pub fn no_clients(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::NoClients, message)
    }

    pub fn kind(&self) -> ErrorKind {
        self.kind
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for BridgeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}] {}", self.kind, self.message)
    }
}

#[cfg(feature = "std")]
impl std::error::Error for BridgeError {}

impl From<serde_json::Error> for BridgeError {
    fn from(e: serde_json::Error) -> Self {
        Self::protocol(alloc::format!("JSON error: {e}"))
    }
}
