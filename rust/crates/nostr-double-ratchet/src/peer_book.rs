use crate::{DeviceId, DomainError, ProtocolContext, Result, Session, SessionState, UnixSeconds};
use rand::{CryptoRng, RngCore};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoredPeerDevice {
    pub device_id: DeviceId,
    pub active_session: Option<SessionState>,
    pub inactive_sessions: Vec<SessionState>,
    pub created_at: UnixSeconds,
    pub is_stale: bool,
    pub stale_timestamp: Option<UnixSeconds>,
    pub last_activity: Option<UnixSeconds>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoredPeerBook {
    pub devices: Vec<StoredPeerDevice>,
}

#[derive(Debug, Clone)]
pub struct PeerBook {
    device_records: BTreeMap<DeviceId, PeerDeviceState>,
}

#[derive(Debug, Clone)]
struct PeerDeviceState {
    device_id: DeviceId,
    active_session: Option<Session>,
    inactive_sessions: Vec<Session>,
    created_at: UnixSeconds,
    is_stale: bool,
    stale_timestamp: Option<UnixSeconds>,
    last_activity: Option<UnixSeconds>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerBookSendPlan {
    pub device_id: DeviceId,
}

#[derive(Debug, Clone)]
pub struct PeerBookReceivePlan {
    pub device_id: DeviceId,
    pub active_match: bool,
    pub inactive_index: Option<usize>,
    pub session_plan: crate::session::ReceivePlan,
}

impl PeerBook {
    pub fn new() -> Self {
        Self {
            device_records: BTreeMap::new(),
        }
    }

    pub fn upsert_session(
        &mut self,
        device_id: Option<DeviceId>,
        session: Session,
        now: UnixSeconds,
    ) {
        let device_id = device_id.unwrap_or_else(|| DeviceId::new("unknown"));
        let device = self
            .device_records
            .entry(device_id.clone())
            .or_insert_with(|| PeerDeviceState {
                device_id: device_id.clone(),
                active_session: None,
                inactive_sessions: Vec::new(),
                created_at: now,
                is_stale: false,
                stale_timestamp: None,
                last_activity: Some(now),
            });

        if Self::device_contains_session_state(device, &session.state) {
            Self::compact_duplicate_sessions(device);
            device.last_activity = Some(now);
            return;
        }

        let new_priority = Self::session_priority(&session);
        let old_priority = device
            .active_session
            .as_ref()
            .map(Self::session_priority)
            .unwrap_or((0, 0, 0));

        if let Some(old_session) = device.active_session.take() {
            if old_priority >= new_priority {
                device.inactive_sessions.push(session);
                device.active_session = Some(old_session);
            } else {
                device.inactive_sessions.push(old_session);
                device.active_session = Some(session);
            }
        } else {
            device.active_session = Some(session);
        }

        Self::compact_duplicate_sessions(device);
        const MAX_INACTIVE: usize = 10;
        if device.inactive_sessions.len() > MAX_INACTIVE {
            device.inactive_sessions.truncate(MAX_INACTIVE);
        }
        device.last_activity = Some(now);
    }

    pub fn plan_send(&self) -> Option<PeerBookSendPlan> {
        self.device_records
            .iter()
            .filter(|(_, device)| !device.is_stale)
            .filter_map(|(device_id, device)| {
                let session = device.active_session.as_ref()?;
                if !session.can_send() {
                    return None;
                }
                Some((device_id.clone(), Self::session_priority(session)))
            })
            .max_by(|(_, left), (_, right)| left.cmp(right))
            .map(|(device_id, _)| PeerBookSendPlan { device_id })
    }

    pub fn apply_send<F, T>(
        &mut self,
        now: UnixSeconds,
        plan: &PeerBookSendPlan,
        mut f: F,
    ) -> Result<T>
    where
        F: FnMut(&mut Session) -> Result<T>,
    {
        let device = self
            .device_records
            .get_mut(&plan.device_id)
            .ok_or_else(|| DomainError::InvalidState("missing send device".to_string()))?;
        let session = device
            .active_session
            .as_mut()
            .ok_or_else(|| DomainError::InvalidState("missing active session".to_string()))?;
        let output = f(session)?;
        device.last_activity = Some(now);
        Ok(output)
    }

    pub fn plan_receive<R>(
        &self,
        ctx: &mut ProtocolContext<'_, R>,
        envelope: &crate::session::IncomingDirectMessageEnvelope,
        preferred_device_id: Option<&DeviceId>,
    ) -> Result<Option<PeerBookReceivePlan>>
    where
        R: RngCore + CryptoRng,
    {
        for device_id in self.prioritized_device_ids(preferred_device_id) {
            let Some(device) = self.device_records.get(&device_id) else {
                continue;
            };
            if device.is_stale {
                continue;
            }

            if let Some(session) = device.active_session.as_ref() {
                if session.matches_sender(envelope.sender) {
                    let session_plan = session.plan_receive(ctx, envelope)?;
                    return Ok(Some(PeerBookReceivePlan {
                        device_id,
                        active_match: true,
                        inactive_index: None,
                        session_plan,
                    }));
                }
            }

            for (index, session) in device.inactive_sessions.iter().enumerate() {
                if !session.matches_sender(envelope.sender) {
                    continue;
                }
                let session_plan = session.plan_receive(ctx, envelope)?;
                return Ok(Some(PeerBookReceivePlan {
                    device_id: device.device_id.clone(),
                    active_match: false,
                    inactive_index: Some(index),
                    session_plan,
                }));
            }
        }

        Ok(None)
    }

    pub fn commit_receive(
        &mut self,
        now: UnixSeconds,
        plan: PeerBookReceivePlan,
    ) -> Result<crate::session::ReceiveOutcome> {
        let device = self
            .device_records
            .get_mut(&plan.device_id)
            .ok_or_else(|| DomainError::InvalidState("missing receive device".to_string()))?;

        let outcome = if plan.active_match {
            let session = device
                .active_session
                .as_mut()
                .ok_or_else(|| DomainError::InvalidState("missing active session".to_string()))?;
            session.apply_receive(plan.session_plan)
        } else {
            let index = plan
                .inactive_index
                .ok_or_else(|| DomainError::InvalidState("missing inactive index".to_string()))?;
            if index >= device.inactive_sessions.len() {
                return Err(
                    DomainError::InvalidState("inactive index out of bounds".to_string()).into(),
                );
            }
            let mut session = device.inactive_sessions.remove(index);
            let outcome = session.apply_receive(plan.session_plan);
            match device.active_session.take() {
                Some(active) => {
                    if Self::session_priority(&session) >= Self::session_priority(&active) {
                        device.inactive_sessions.push(active);
                        device.active_session = Some(session);
                    } else {
                        device.inactive_sessions.push(session);
                        device.active_session = Some(active);
                    }
                }
                None => {
                    device.active_session = Some(session);
                }
            }
            Self::compact_duplicate_sessions(device);
            outcome
        };

        device.last_activity = Some(now);
        Ok(outcome)
    }

    pub fn snapshot(&self) -> StoredPeerBook {
        StoredPeerBook {
            devices: self
                .device_records
                .values()
                .map(|device| StoredPeerDevice {
                    device_id: device.device_id.clone(),
                    active_session: device
                        .active_session
                        .as_ref()
                        .map(|session| session.state.clone()),
                    inactive_sessions: device
                        .inactive_sessions
                        .iter()
                        .map(|session| session.state.clone())
                        .collect(),
                    created_at: device.created_at,
                    is_stale: device.is_stale,
                    stale_timestamp: device.stale_timestamp,
                    last_activity: device.last_activity,
                })
                .collect(),
        }
    }

    fn prioritized_device_ids(&self, preferred_device_id: Option<&DeviceId>) -> Vec<DeviceId> {
        let mut scored: Vec<(DeviceId, (u8, u32, u32))> = self
            .device_records
            .iter()
            .filter(|(_, device)| !device.is_stale)
            .map(|(device_id, device)| {
                let score = device
                    .active_session
                    .as_ref()
                    .map(Self::session_priority)
                    .unwrap_or((0, 0, 0));
                (device_id.clone(), score)
            })
            .collect();
        scored.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));

        let mut ordered: Vec<DeviceId> =
            scored.into_iter().map(|(device_id, _)| device_id).collect();
        if let Some(preferred) = preferred_device_id {
            if let Some(index) = ordered.iter().position(|device_id| device_id == preferred) {
                let preferred = ordered.remove(index);
                ordered.insert(0, preferred);
            }
        }

        ordered
    }

    fn session_priority(session: &Session) -> (u8, u32, u32) {
        let can_send = session.can_send();
        let can_receive = session.state.receiving_chain_key.is_some()
            || session.state.their_current_nostr_public_key.is_some()
            || session.state.receiving_chain_message_number > 0;

        let directionality = match (can_send, can_receive) {
            (true, true) => 3,
            (true, false) => 2,
            (false, true) => 1,
            (false, false) => 0,
        };

        (
            directionality,
            session.state.receiving_chain_message_number,
            session.state.sending_chain_message_number,
        )
    }

    fn device_contains_session_state(device: &PeerDeviceState, state: &SessionState) -> bool {
        device
            .active_session
            .as_ref()
            .is_some_and(|session| session.state == *state)
            || device
                .inactive_sessions
                .iter()
                .any(|session| session.state == *state)
    }

    fn compact_duplicate_sessions(device: &mut PeerDeviceState) {
        let active_state = device
            .active_session
            .as_ref()
            .map(|session| session.state.clone());
        let mut unique_states = Vec::new();
        let mut inactive_sessions = Vec::with_capacity(device.inactive_sessions.len());

        for session in device.inactive_sessions.drain(..) {
            let is_duplicate = active_state
                .as_ref()
                .is_some_and(|state| *state == session.state)
                || unique_states
                    .iter()
                    .any(|state: &SessionState| *state == session.state);
            if is_duplicate {
                continue;
            }
            unique_states.push(session.state.clone());
            inactive_sessions.push(session);
        }

        device.inactive_sessions = inactive_sessions;
    }
}

impl Default for PeerBook {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        device_pubkey_from_secret_bytes, DirectMessageContent, IncomingDirectMessageEnvelope,
        ProtocolContext, Rumor, Session, UnixMillis,
    };
    use rand::{rngs::StdRng, SeedableRng};

    fn context(seed: u64) -> ProtocolContext<'static, StdRng> {
        let rng = Box::new(StdRng::seed_from_u64(seed));
        let rng = Box::leak(rng);
        ProtocolContext::new(
            UnixSeconds(1_700_000_100),
            UnixMillis(1_700_000_100_000),
            rng,
        )
    }

    fn session_pair() -> (DeviceId, Session, Session, IncomingDirectMessageEnvelope) {
        let alice_secret = [11u8; 32];
        let bob_secret = [12u8; 32];
        let alice_pub = device_pubkey_from_secret_bytes(&alice_secret).unwrap();
        let bob_pub = device_pubkey_from_secret_bytes(&bob_secret).unwrap();
        let shared_secret = [13u8; 32];

        let mut alice_init = context(1);
        let alice = Session::init(
            &mut alice_init,
            bob_pub,
            alice_secret,
            true,
            shared_secret,
            None,
        )
        .unwrap();
        let mut bob_init = context(2);
        let bob = Session::init(
            &mut bob_init,
            alice_pub,
            bob_secret,
            false,
            shared_secret,
            None,
        )
        .unwrap();

        let mut send_ctx = context(3);
        let rumor =
            Rumor::from_content(&mut send_ctx, DirectMessageContent::Text("hi".to_string()))
                .unwrap();
        let send_plan = alice.plan_send(&rumor, send_ctx.now_secs).unwrap();
        let envelope = alice.clone().apply_send(send_plan).envelope;
        (
            DeviceId::new("device-a"),
            alice,
            bob,
            IncomingDirectMessageEnvelope {
                sender: envelope.sender,
                created_at: envelope.created_at,
                encrypted_header: envelope.encrypted_header,
                ciphertext: envelope.ciphertext,
            },
        )
    }

    #[test]
    fn send_selection_is_deterministic() {
        let (device_id, alice, _, _) = session_pair();
        let mut book = PeerBook::new();
        book.upsert_session(
            Some(DeviceId::new("device-b")),
            alice.clone(),
            UnixSeconds(10),
        );
        book.upsert_session(Some(device_id.clone()), alice, UnixSeconds(10));

        let plan = book.plan_send().expect("send plan");
        assert_eq!(plan.device_id, DeviceId::new("device-b"));
    }

    #[test]
    fn inactive_match_is_promoted_on_receive() {
        let (device_id, _alice, bob, incoming) = session_pair();
        let mut book = PeerBook::new();
        book.upsert_session(Some(device_id.clone()), bob, UnixSeconds(10));

        {
            let device = book.device_records.get_mut(&device_id).unwrap();
            let matching_session = device.active_session.take().unwrap();

            let unrelated_sender_secret = [99u8; 32];
            let unrelated_private_secret = [98u8; 32];
            let unrelated_sender =
                device_pubkey_from_secret_bytes(&unrelated_sender_secret).unwrap();
            let mut init_ctx = context(40);
            let unrelated_session = Session::init(
                &mut init_ctx,
                unrelated_sender,
                unrelated_private_secret,
                false,
                [13u8; 32],
                None,
            )
            .unwrap();

            device.active_session = Some(unrelated_session);
            device.inactive_sessions.push(matching_session);
        }

        let mut recv_ctx = context(4);
        let plan = book
            .plan_receive(&mut recv_ctx, &incoming, Some(&device_id))
            .unwrap()
            .expect("receive plan");
        assert!(!plan.active_match);

        let _ = book.commit_receive(UnixSeconds(20), plan).unwrap();
        let device = book.device_records.get(&device_id).unwrap();
        assert!(device
            .active_session
            .as_ref()
            .unwrap()
            .state
            .receiving_chain_key
            .is_some());
    }

    #[test]
    fn failed_plan_does_not_mutate_book() {
        let (device_id, _alice, bob, incoming) = session_pair();
        let mut book = PeerBook::new();
        book.upsert_session(Some(device_id.clone()), bob.clone(), UnixSeconds(10));
        let before = book.snapshot();

        let mut recv_ctx = context(5);
        let result = book.plan_receive(
            &mut recv_ctx,
            &IncomingDirectMessageEnvelope {
                sender: incoming.sender,
                created_at: incoming.created_at,
                encrypted_header: "bad".to_string(),
                ciphertext: "bad".to_string(),
            },
            Some(&device_id),
        );
        assert!(result.is_err());
        assert_eq!(book.snapshot(), before);
    }
}
