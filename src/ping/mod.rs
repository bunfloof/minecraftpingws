mod bedrock;
mod java;

pub use bedrock::BedrockPinger;
pub use java::JavaPinger;

use std::fmt;

#[derive(Debug)]
pub enum PingError {
    Timeout(String),
    ConnectionRefused,
    DnsError(String),
    IoError(String),
    ParseError(String),
}

impl fmt::Display for PingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PingError::Timeout(msg) => write!(f, "Request timed out: {}", msg),
            PingError::ConnectionRefused => write!(f, "Connection refused"),
            PingError::DnsError(msg) => write!(f, "DNS resolution failed: {}", msg),
            PingError::IoError(msg) => write!(f, "IO error: {}", msg),
            PingError::ParseError(msg) => write!(f, "Parse error: {}", msg),
        }
    }
}

impl std::error::Error for PingError {}

impl From<std::io::Error> for PingError {
    fn from(err: std::io::Error) -> Self {
        match err.kind() {
            std::io::ErrorKind::ConnectionRefused => PingError::ConnectionRefused,
            std::io::ErrorKind::TimedOut => PingError::Timeout(err.to_string()),
            _ => PingError::IoError(err.to_string()),
        }
    }
}
