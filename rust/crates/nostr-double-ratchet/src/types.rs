use crate::ids::{UnixMillis, UnixSeconds};
use rand::{CryptoRng, RngCore};

pub const MESSAGE_EVENT_KIND: u32 = 1060;
pub const INVITE_EVENT_KIND: u32 = 30078;
pub const APP_KEYS_EVENT_KIND: u32 = 30078;
pub const INVITE_RESPONSE_KIND: u32 = 1059;
pub const CHAT_MESSAGE_KIND: u32 = 14;
pub const CHAT_SETTINGS_KIND: u32 = 10448;
pub const REACTION_KIND: u32 = 7;
pub const RECEIPT_KIND: u32 = 15;
pub const TYPING_KIND: u32 = 25;
pub const SHARED_CHANNEL_KIND: u32 = 4;
pub const MAX_SKIP: usize = 1000;
pub const EXPIRATION_TAG: &str = "expiration";

pub struct ProtocolContext<'a, R>
where
    R: RngCore + CryptoRng,
{
    pub now_secs: UnixSeconds,
    pub now_millis: UnixMillis,
    pub rng: &'a mut R,
}

impl<'a, R> ProtocolContext<'a, R>
where
    R: RngCore + CryptoRng,
{
    pub fn new(now_secs: UnixSeconds, now_millis: UnixMillis, rng: &'a mut R) -> Self {
        Self {
            now_secs,
            now_millis,
            rng,
        }
    }
}
