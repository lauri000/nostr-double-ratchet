use thiserror::Error;

#[derive(Error, Debug, Clone, PartialEq, Eq)]
pub enum DomainError {
    #[error("Too many skipped messages")]
    TooManySkippedMessages,

    #[error("Not initiator, cannot send first message")]
    NotInitiator,

    #[error("Session not ready")]
    SessionNotReady,

    #[error("Device ID required")]
    DeviceIdRequired,

    #[error("Unknown peer: {0}")]
    UnknownPeer(String),

    #[error("No sendable session for peer: {0}")]
    NoSendableSession(String),

    #[error("Unexpected sender for session")]
    UnexpectedSender,

    #[error("Invite exhausted")]
    InviteExhausted,

    #[error("Invite already used")]
    InviteAlreadyUsed,

    #[error("Invalid state: {0}")]
    InvalidState(String),
}

#[derive(Error, Debug, Clone, PartialEq, Eq)]
pub enum CodecError {
    #[error("Invalid event: {0}")]
    InvalidEvent(String),

    #[error("Invalid header")]
    InvalidHeader,

    #[error("Invite codec error: {0}")]
    Invite(String),

    #[error("Parse error: {0}")]
    Parse(String),
}

#[derive(Error, Debug)]
pub enum Error {
    #[error(transparent)]
    Domain(#[from] DomainError),

    #[error(transparent)]
    Codec(#[from] CodecError),

    #[error("Encryption error: {0}")]
    Encryption(String),

    #[error("Decryption error: {0}")]
    Decryption(String),

    #[error(transparent)]
    NostrKey(#[from] nostr::key::Error),

    #[error(transparent)]
    Nostr(#[from] nostr::event::Error),

    #[error(transparent)]
    UnsignedEvent(#[from] nostr::event::unsigned::Error),

    #[error(transparent)]
    Nip44(#[from] nostr::nips::nip44::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

impl From<serde_json::Error> for Error {
    fn from(value: serde_json::Error) -> Self {
        CodecError::Parse(value.to_string()).into()
    }
}

impl From<hex::FromHexError> for Error {
    fn from(value: hex::FromHexError) -> Self {
        CodecError::Parse(value.to_string()).into()
    }
}
