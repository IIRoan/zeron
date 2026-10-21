//! Broker only: the connected desktop owns WebKit, tab consent and execution.
use futures::{StreamExt, stream, stream::BoxStream};
use serde_json::Value;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::{mpsc, oneshot};
use zeron_proto::browser::*;
use zeron_rpc::RpcError;

#[derive(Clone, Default)]
pub struct BrowserBroker(Arc<Mutex<State>>);

#[derive(Default)]
struct State {
    hosts: HashMap<String, Host>,
    pending: HashMap<String, (String, oneshot::Sender<Result<Value, String>>)>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tab() -> BrowserTab {
        BrowserTab {
            id: "host/1".into(),
            chat_id: "chat".into(),
            title: "Page".into(),
            url: Some("https://example.test/".into()),
            loading: false,
            enabled: true,
        }
    }
    fn request() -> BrowserRequest {
        BrowserRequest {
            chat_id: "chat".into(),
            browser_id: "host/1".into(),
            command: BrowserAction::Snapshot,
        }
    }
    async fn host(broker: &BrowserBroker) -> BoxStream<'static, Value> {
        let mut host = broker.attach("host".into()).unwrap();
        assert_eq!(host.next().await.unwrap()["ready"], true);
        broker.update("host", 1, vec![tab()]).unwrap();
        host
    }

    #[tokio::test]
    async fn routes_only_to_the_owning_desktop_and_chat() {
        let broker = BrowserBroker::default();
        let mut host = host(&broker).await;
        assert!(broker.list("other").is_empty());
        let mut wrong = request();
        wrong.chat_id = "other".into();
        assert!(broker.execute(wrong).await.is_err());
        let running = tokio::spawn({
            let broker = broker.clone();
            async move { broker.execute(request()).await }
        });
        let event: BrowserHostRequest = serde_json::from_value(host.next().await.unwrap()).unwrap();
        let mut reply = BrowserHostReply {
            host_id: "impostor".into(),
            request_id: event.request_id,
            result: Ok(serde_json::json!({"title":"hello"})),
        };
        assert!(broker.reply(reply.clone()).is_err());
        assert!(!running.is_finished());
        reply.host_id = "host".into();
        broker.reply(reply).unwrap();
        assert_eq!(running.await.unwrap().unwrap()["title"], "hello");
        assert!(broker.0.lock().unwrap().pending.is_empty());
    }

    #[tokio::test]
    async fn revocation_cannot_be_overwritten_by_an_old_inventory() {
        let broker = BrowserBroker::default();
        let _host = host(&broker).await;
        let mut disabled = tab();
        disabled.enabled = false;
        broker.update("host", 3, vec![disabled]).unwrap();
        broker.update("host", 2, vec![tab()]).unwrap();
        assert!(broker.execute(request()).await.is_err());
        assert!(broker.0.lock().unwrap().pending.is_empty());
    }

    #[tokio::test]
    async fn disconnect_fails_pending_calls_and_removes_tabs() {
        let broker = BrowserBroker::default();
        let mut host = host(&broker).await;
        let running = tokio::spawn({
            let broker = broker.clone();
            async move { broker.execute(request()).await }
        });
        host.next().await.unwrap();
        drop(host);
        assert!(
            running
                .await
                .unwrap()
                .unwrap_err()
                .to_string()
                .contains("browser_no_host")
        );
        assert!(broker.list("chat").is_empty());
        assert!(broker.0.lock().unwrap().pending.is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn timed_out_and_cancelled_requests_release_pending_slots() {
        let broker = BrowserBroker::default();
        let mut host = host(&broker).await;
        let running = tokio::spawn({
            let broker = broker.clone();
            async move { broker.execute(request()).await }
        });
        host.next().await.unwrap();
        tokio::time::advance(Duration::from_secs(21)).await;
        assert!(
            running
                .await
                .unwrap()
                .unwrap_err()
                .to_string()
                .contains("browser_timeout")
        );
        let running = tokio::spawn({
            let broker = broker.clone();
            async move { broker.execute(request()).await }
        });
        host.next().await.unwrap();
        running.abort();
        let _ = running.await;
        assert!(broker.0.lock().unwrap().pending.is_empty());
    }

    #[tokio::test]
    async fn rejects_unsafe_navigation_and_oversized_commands() {
        let broker = BrowserBroker::default();
        let _host = host(&broker).await;
        for url in [
            "javascript:alert(1)",
            "file:///etc/passwd",
            "https://user:secret@example.test/",
            "localhost:3000",
        ] {
            let mut request = request();
            request.command = BrowserAction::Navigate { url: url.into() };
            assert!(broker.execute(request).await.is_err(), "{url}");
        }
        let mut request = request();
        request.command = BrowserAction::Evaluate {
            script: "x".repeat(300_000),
        };
        assert!(broker.execute(request).await.is_err());
        assert!(broker.0.lock().unwrap().pending.is_empty());
    }
}
struct Host {
    tx: mpsc::Sender<Value>,
    revision: u64,
    tabs: Vec<BrowserTab>,
}

struct HostLease(BrowserBroker, String);
impl Drop for HostLease {
    fn drop(&mut self) {
        let mut state = self.0.0.lock().unwrap();
        state.hosts.remove(&self.1);
        state.pending.retain(|_, (host, _)| host != &self.1);
    }
}
struct PendingLease(BrowserBroker, String);
impl Drop for PendingLease {
    fn drop(&mut self) {
        self.0.0.lock().unwrap().pending.remove(&self.1);
    }
}

impl BrowserBroker {
    pub fn attach(&self, id: String) -> Result<BoxStream<'static, Value>, RpcError> {
        if id.is_empty() || id.len() > 128 {
            return Err(RpcError::BadParams("invalid browser host id".into()));
        }
        let mut state = self.0.lock().unwrap();
        if state.hosts.contains_key(&id) {
            return Err(RpcError::Failed("browser host already attached".into()));
        }
        let (tx, rx) = mpsc::channel(32);
        tx.try_send(serde_json::json!({"ready": true})).unwrap();
        state.hosts.insert(
            id.clone(),
            Host {
                tx,
                revision: 0,
                tabs: vec![],
            },
        );
        Ok(stream::unfold(
            (rx, HostLease(self.clone(), id)),
            |(mut rx, lease)| async move { rx.recv().await.map(|value| (value, (rx, lease))) },
        )
        .boxed())
    }

    pub fn update(&self, id: &str, revision: u64, tabs: Vec<BrowserTab>) -> Result<(), RpcError> {
        if tabs.len() > 512
            || tabs.iter().any(|tab| {
                !tab.id.starts_with(&format!("{id}/"))
                    || tab.id.len() > 256
                    || tab.chat_id.len() > 256
                    || tab.title.len() > 8192
                    || tab.url.as_ref().is_some_and(|url| url.len() > 16384)
            })
        {
            return Err(RpcError::BadParams("invalid browser tab inventory".into()));
        }
        let mut state = self.0.lock().unwrap();
        let host = state
            .hosts
            .get_mut(id)
            .ok_or_else(|| RpcError::Failed("browser host disconnected".into()))?;
        if revision > host.revision {
            host.revision = revision;
            host.tabs = tabs;
        }
        Ok(())
    }

    pub fn list(&self, chat: &str) -> Vec<BrowserTab> {
        let state = self.0.lock().unwrap();
        let mut tabs: Vec<_> = state
            .hosts
            .values()
            .flat_map(|host| &host.tabs)
            .filter(|tab| tab.chat_id == chat)
            .cloned()
            .collect();
        tabs.sort_by(|a, b| a.id.cmp(&b.id));
        tabs
    }

    pub async fn execute(&self, request: BrowserRequest) -> Result<Value, RpcError> {
        if serde_json::to_vec(&request).map_or(true, |data| data.len() > 256 * 1024) {
            return Err(RpcError::BadParams("browser command is too large".into()));
        }
        if let BrowserAction::Navigate { url } | BrowserAction::NewTab { url } = &request.command {
            let url = reqwest::Url::parse(url)
                .map_err(|_| RpcError::BadParams("browser URL must be http or https".into()))?;
            if !matches!(url.scheme(), "http" | "https")
                || url.host_str().is_none()
                || !url.username().is_empty()
                || url.password().is_some()
            {
                return Err(RpcError::BadParams(
                    "browser URL must be http or https without embedded credentials".into(),
                ));
            }
        }
        let request_id = uuid::Uuid::new_v4().to_string();
        let (tx, rx) = oneshot::channel();
        {
            let mut state = self.0.lock().unwrap();
            let (host_id, host) = state
                .hosts
                .iter()
                .find(|(_, host)| {
                    host.tabs.iter().any(|tab| {
                        tab.id == request.browser_id
                            && tab.chat_id == request.chat_id
                            && tab.enabled
                    })
                })
                .ok_or_else(|| {
                    RpcError::Failed(
                        "browser_unavailable: enable agent control on a browser tab in this chat"
                            .into(),
                    )
                })?;
            let host_id = host_id.clone();
            let value = serde_json::to_value(BrowserHostRequest {
                request_id: request_id.clone(),
                request,
            })
            .unwrap();
            if state.pending.len() >= 128 {
                return Err(RpcError::Failed(
                    "browser_busy: too many pending requests".into(),
                ));
            }
            host.tx
                .try_send(value)
                .map_err(|_| RpcError::Failed("browser_busy: desktop unavailable".into()))?;
            state.pending.insert(request_id.clone(), (host_id, tx));
        }
        let _pending = PendingLease(self.clone(), request_id);
        tokio::time::timeout(Duration::from_secs(20), rx).await
            .map_err(|_| RpcError::Failed("browser_timeout: desktop did not respond; inspect the page before retrying an action".into()))?
            .map_err(|_| RpcError::Failed("browser_no_host: desktop disconnected".into()))?
            .map_err(RpcError::Failed)
    }

    pub fn reply(&self, reply: BrowserHostReply) -> Result<(), RpcError> {
        let mut state = self.0.lock().unwrap();
        if let Some((host, _)) = state.pending.get(&reply.request_id) {
            if host != &reply.host_id {
                return Err(RpcError::BadParams(
                    "browser reply belongs to another desktop".into(),
                ));
            }
            if let Some((_, tx)) = state.pending.remove(&reply.request_id) {
                let _ = tx.send(reply.result);
            }
        }
        Ok(())
    }
}
