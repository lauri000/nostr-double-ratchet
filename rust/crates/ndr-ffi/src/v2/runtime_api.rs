use crate::NdrError;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, uniffi::Record, PartialEq, Eq)]
pub struct RuntimeEffect {
    pub kind: String,
    pub request_id: Option<String>,
    pub sub_id: Option<String>,
    pub correlation_id: Option<String>,
    pub event_json: Option<String>,
    pub key: Option<String>,
    pub value: Option<String>,
    pub success: Option<bool>,
    pub error: Option<String>,
}

#[derive(Default)]
struct RuntimeState {
    started: bool,
    effects: VecDeque<RuntimeEffect>,
    request_to_sub: HashMap<String, String>,
    active_subscriptions: HashSet<String>,
    router: SubscriptionRouter,
    app_keys: AppKeysNode,
    sessions: SessionsNode,
}

#[derive(uniffi::Object)]
pub struct RuntimeApi {
    inner: Mutex<RuntimeState>,
}

#[uniffi::export]
impl RuntimeApi {
    #[uniffi::constructor]
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(RuntimeState::default()),
        })
    }

    pub fn start(&self) {
        let mut state = self.inner.lock().expect("runtime mutex poisoned");
        if state.started {
            return;
        }
        state.started = true;
        state.effects.push_back(RuntimeEffect {
            kind: "runtime.started".to_string(),
            request_id: None,
            sub_id: None,
            correlation_id: None,
            event_json: None,
            key: None,
            value: None,
            success: Some(true),
            error: None,
        });
    }

    pub fn on_subscription_opened(
        &self,
        request_id: String,
        sub_id: String,
    ) -> Result<(), NdrError> {
        let mut state = self.inner.lock().expect("runtime mutex poisoned");
        ensure_started(&state)?;

        state
            .request_to_sub
            .insert(request_id.clone(), sub_id.clone());
        state.active_subscriptions.insert(sub_id.clone());
        if let Some(route) = Route::from_request_id(&request_id) {
            state.router.bind(sub_id.clone(), route);
        }
        state.effects.push_back(RuntimeEffect {
            kind: "runtime.nostr.subscription_opened".to_string(),
            request_id: Some(request_id),
            sub_id: Some(sub_id),
            correlation_id: None,
            event_json: None,
            key: None,
            value: None,
            success: Some(true),
            error: None,
        });
        Ok(())
    }

    pub fn on_subscription_closed(&self, sub_id: String) -> Result<(), NdrError> {
        let mut state = self.inner.lock().expect("runtime mutex poisoned");
        ensure_started(&state)?;

        state.active_subscriptions.remove(&sub_id);
        state
            .request_to_sub
            .retain(|_, mapped_sub_id| mapped_sub_id != &sub_id);
        state.router.unbind(&sub_id);
        state.effects.push_back(RuntimeEffect {
            kind: "runtime.nostr.subscription_closed".to_string(),
            request_id: None,
            sub_id: Some(sub_id),
            correlation_id: None,
            event_json: None,
            key: None,
            value: None,
            success: Some(true),
            error: None,
        });
        Ok(())
    }

    pub fn on_nostr_event(&self, sub_id: String, event_json: String) -> Result<(), NdrError> {
        let mut state = self.inner.lock().expect("runtime mutex poisoned");
        ensure_started(&state)?;

        let event = NostrEvent::from_json(event_json)?;
        let Some(route) = state.router.resolve(&sub_id) else {
            state.effects.push_back(RuntimeEffect {
                kind: "runtime.nostr.unrouted_event".to_string(),
                request_id: None,
                sub_id: Some(sub_id),
                correlation_id: None,
                event_json: Some(event.normalized_json),
                key: None,
                value: None,
                success: Some(false),
                error: Some("unknown subscription route".to_string()),
            });
            return Ok(());
        };

        let effects = match route {
            Route::AppKeys(app_route) => state.app_keys.on_nostr_event(app_route, &event),
            Route::Sessions(session_route) => state.sessions.on_nostr_event(session_route, &event),
        };
        apply_manager_effects(&mut state, effects);

        state.effects.push_back(RuntimeEffect {
            kind: "runtime.nostr.event_routed".to_string(),
            request_id: None,
            sub_id: Some(sub_id),
            correlation_id: None,
            event_json: Some(event.normalized_json),
            key: None,
            value: None,
            success: Some(true),
            error: None,
        });
        Ok(())
    }

    pub fn on_publish_result(
        &self,
        correlation_id: String,
        success: bool,
        error: Option<String>,
    ) -> Result<(), NdrError> {
        let mut state = self.inner.lock().expect("runtime mutex poisoned");
        ensure_started(&state)?;

        state.effects.push_back(RuntimeEffect {
            kind: "runtime.nostr.publish_result".to_string(),
            request_id: None,
            sub_id: None,
            correlation_id: Some(correlation_id),
            event_json: None,
            key: None,
            value: None,
            success: Some(success),
            error,
        });
        Ok(())
    }

    pub fn on_storage_read_result(
        &self,
        correlation_id: String,
        key: String,
        value: Option<String>,
        error: Option<String>,
    ) -> Result<(), NdrError> {
        let mut state = self.inner.lock().expect("runtime mutex poisoned");
        ensure_started(&state)?;

        state.effects.push_back(RuntimeEffect {
            kind: "runtime.storage.read_result".to_string(),
            request_id: None,
            sub_id: None,
            correlation_id: Some(correlation_id),
            event_json: None,
            key: Some(key),
            value,
            success: Some(error.is_none()),
            error,
        });
        Ok(())
    }

    pub fn on_storage_write_result(
        &self,
        correlation_id: String,
        key: String,
        success: bool,
        error: Option<String>,
    ) -> Result<(), NdrError> {
        let mut state = self.inner.lock().expect("runtime mutex poisoned");
        ensure_started(&state)?;

        state.effects.push_back(RuntimeEffect {
            kind: "runtime.storage.write_result".to_string(),
            request_id: None,
            sub_id: None,
            correlation_id: Some(correlation_id),
            event_json: None,
            key: Some(key),
            value: None,
            success: Some(success),
            error,
        });
        Ok(())
    }

    pub fn on_storage_delete_result(
        &self,
        correlation_id: String,
        key: String,
        success: bool,
        error: Option<String>,
    ) -> Result<(), NdrError> {
        let mut state = self.inner.lock().expect("runtime mutex poisoned");
        ensure_started(&state)?;

        state.effects.push_back(RuntimeEffect {
            kind: "runtime.storage.delete_result".to_string(),
            request_id: None,
            sub_id: None,
            correlation_id: Some(correlation_id),
            event_json: None,
            key: Some(key),
            value: None,
            success: Some(success),
            error,
        });
        Ok(())
    }

    pub fn drain_effects(&self) -> Vec<RuntimeEffect> {
        let mut state = self.inner.lock().expect("runtime mutex poisoned");
        state.effects.drain(..).collect()
    }
}

#[derive(Debug, Clone)]
struct NostrEvent {
    normalized_json: String,
}

impl NostrEvent {
    fn from_json(event_json: String) -> Result<Self, NdrError> {
        let parsed: nostr::Event =
            serde_json::from_str(&event_json).map_err(|e| NdrError::InvalidEvent(e.to_string()))?;
        let normalized_json =
            serde_json::to_string(&parsed).map_err(|e| NdrError::Serialization(e.to_string()))?;
        Ok(Self { normalized_json })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Route {
    AppKeys(AppKeysRoute),
    Sessions(SessionsRoute),
}

impl Route {
    fn from_request_id(request_id: &str) -> Option<Self> {
        if request_id.starts_with("app_keys:") || request_id.starts_with("app_keys/") {
            Some(Route::AppKeys(AppKeysRoute::Default))
        } else if request_id.starts_with("sessions:") || request_id.starts_with("sessions/") {
            Some(Route::Sessions(SessionsRoute::Default))
        } else {
            None
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AppKeysRoute {
    Default,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionsRoute {
    Default,
}

#[derive(Default)]
struct SubscriptionRouter {
    by_sub_id: HashMap<String, Route>,
}

impl SubscriptionRouter {
    fn bind(&mut self, sub_id: String, route: Route) {
        self.by_sub_id.insert(sub_id, route);
    }

    fn unbind(&mut self, sub_id: &str) {
        self.by_sub_id.remove(sub_id);
    }

    fn resolve(&self, sub_id: &str) -> Option<Route> {
        self.by_sub_id.get(sub_id).copied()
    }
}

enum ManagerEffect {}

#[derive(Default)]
struct AppKeysNode;

impl AppKeysNode {
    fn on_nostr_event(&mut self, _route: AppKeysRoute, _event: &NostrEvent) -> Vec<ManagerEffect> {
        // Intentionally no-op in this slice: runtime routing only.
        Vec::new()
    }
}

#[derive(Default)]
struct SessionsNode;

impl SessionsNode {
    fn on_nostr_event(&mut self, _route: SessionsRoute, _event: &NostrEvent) -> Vec<ManagerEffect> {
        // Intentionally no-op in this slice: runtime routing only.
        Vec::new()
    }
}

fn apply_manager_effects(_state: &mut RuntimeState, effects: Vec<ManagerEffect>) {
    for effect in effects {
        match effect {}
    }
}

fn ensure_started(state: &RuntimeState) -> Result<(), NdrError> {
    if state.started {
        Ok(())
    } else {
        Err(NdrError::StateMismatch(
            "RuntimeApi.start() must be called first".to_string(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::RuntimeApi;

    #[test]
    fn callbacks_require_start() {
        let api = RuntimeApi::new();
        let err = api
            .on_subscription_opened("req-1".to_string(), "sub-1".to_string())
            .expect_err("callbacks should reject calls before start");
        match err {
            crate::NdrError::StateMismatch(msg) => {
                assert!(msg.contains("start"), "unexpected message: {msg}")
            }
            _ => panic!("unexpected error type"),
        }
    }

    #[test]
    fn drains_effects_in_fifo_order() {
        let api = RuntimeApi::new();
        api.start();
        api.on_subscription_opened("app_keys:req-1".to_string(), "sub-1".to_string())
            .expect("open callback should succeed");

        let keys = nostr::Keys::generate();
        let unsigned = nostr::EventBuilder::text_note("hello").build(keys.public_key());
        let signed = unsigned
            .sign_with_keys(&keys)
            .expect("event signing should succeed");
        let event_json =
            serde_json::to_string(&signed).expect("signed event should serialize");

        api.on_nostr_event("sub-1".to_string(), event_json)
            .expect("event callback should succeed");
        api.on_storage_read_result(
            "corr-1".to_string(),
            "app-keys".to_string(),
            Some("{}".to_string()),
            None,
        )
        .expect("storage callback should succeed");

        let effects = api.drain_effects();
        let kinds = effects
            .iter()
            .map(|effect| effect.kind.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            kinds,
            vec![
                "runtime.started",
                "runtime.nostr.subscription_opened",
                "runtime.nostr.event_routed",
                "runtime.storage.read_result",
            ]
        );

        assert!(
            api.drain_effects().is_empty(),
            "drain should clear the queue"
        );
    }

    #[test]
    fn unrouted_sub_id_emits_unrouted_effect() {
        let api = RuntimeApi::new();
        api.start();

        let keys = nostr::Keys::generate();
        let unsigned = nostr::EventBuilder::text_note("hello").build(keys.public_key());
        let signed = unsigned
            .sign_with_keys(&keys)
            .expect("event signing should succeed");
        let event_json =
            serde_json::to_string(&signed).expect("signed event should serialize");

        api.on_nostr_event("sub-missing".to_string(), event_json)
            .expect("callback should not error for unrouted sub_id");

        let effects = api.drain_effects();
        let last = effects
            .last()
            .expect("expected at least one effect after start + event");
        assert_eq!(last.kind, "runtime.nostr.unrouted_event");
        assert_eq!(last.success, Some(false));
    }
}
