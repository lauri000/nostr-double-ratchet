use thiserror::Error;

#[derive(Error, Debug, Clone, PartialEq, Eq)]
pub enum DomainError {
    #[error("too many skipped messages")]
    TooManySkippedMessages,

    #[error("session not ready")]
    SessionNotReady,

    #[error("session cannot send yet")]
    CannotSendYet,

    #[error("unexpected sender for session")]
    UnexpectedSender,

    #[error("invite exhausted")]
    InviteExhausted,

    #[error("invite already used")]
    InviteAlreadyUsed,

    #[error("invalid group operation: {0}")]
    InvalidGroupOperation(String),

    #[error("invalid state: {0}")]
    InvalidState(String),
}

#[derive(Error, Debug)]
pub enum Error {
    #[error(transparent)]
    Domain(#[from] DomainError),

    #[error("parse error: {0}")]
    Parse(String),

    #[error("encryption error: {0}")]
    Encryption(String),

    #[error("decryption error: {0}")]
    Decryption(String),

    #[error(transparent)]
    NostrKey(#[from] nostr::key::Error),

    #[error(transparent)]
    Nip44(#[from] nostr::nips::nip44::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

impl From<serde_json::Error> for Error {
    fn from(value: serde_json::Error) -> Self {
        Self::Parse(value.to_string())
    }
}

impl From<hex::FromHexError> for Error {
    fn from(value: hex::FromHexError) -> Self {
        Self::Parse(value.to_string())
    }
}
