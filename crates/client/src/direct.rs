//! Direct instance transport. Reuses the normal registry mirror, session renderer
//! and durable outbox; no edge rooms or WorkOS requests are made in this mode.
use crate::{
    client::ClientInner,
    error::{ClientError, Result},
    lock,
    session::SessionCore,
};
use serde_json::{Value, json};
use std::{
    collections::HashSet,
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use zeron_rpc::{RpcClient, methods};

const TIMEOUT: Duration = Duration::from_secs(12);

pub fn endpoint_url(input: &str) -> Result<String> {
    let mut url = reqwest::Url::parse(input.trim()).map_err(|_| {
        ClientError::InvalidArgument("Enter a valid https:// instance address".into())
    })?;
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(ClientError::InvalidArgument(
            "Instance addresses cannot contain credentials, queries or fragments".into(),
        ));
    }
    match url.scheme() {
        "https" => {
            let _ = url.set_scheme("wss");
        }
        "wss" => {}
        "http" | "ws" if matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]")) => {
            let _ = url.set_scheme("ws");
        }
        _ => {
            return Err(ClientError::InvalidArgument(
                "Use HTTPS for remote instances (HTTP is allowed only on localhost)".into(),
            ));
        }
    }
    url.set_path("/rpc");
    Ok(url.to_string())
}

/// Exchanges a one-time key for a phone-specific credential.
pub async fn pair(endpoint: &str, code: &str, name: &str) -> Result<crate::Credentials> {
    let endpoint = endpoint_url(endpoint)?;
    let rpc = zeron_rpc::connect_ws_authorized(&endpoint, Some(&format!("Pair {}", code.trim())))
        .await
        .map_err(rpc_error)?;
    let reply = timed_call(&rpc, "Pair", json!({"name": name})).await?;
    let instance_id = reply["instanceId"]
        .as_str()
        .ok_or_else(|| ClientError::HostError("Invalid pairing response".into()))?
        .to_owned();
    let token = reply["token"]
        .as_str()
        .ok_or_else(|| ClientError::HostError("Invalid pairing credential".into()))?
        .to_owned();
    Ok(crate::Credentials::Direct {
        endpoint,
        instance_id,
        token,
    })
}

fn rpc_error(e: zeron_rpc::RpcError) -> ClientError {
    ClientError::HostUnavailable(e.to_string())
}
async fn timed_call(rpc: &RpcClient, method: &str, params: Value) -> Result<Value> {
    tokio::time::timeout(TIMEOUT, rpc.call(method, params))
        .await
        .map_err(|_| ClientError::HostUnavailable("Instance did not respond".into()))?
        .map_err(rpc_error)
}

pub(crate) struct Direct {
    endpoint: String,
    instance_id: String,
    token: String,
    connection: tokio::sync::Mutex<Option<Arc<RpcClient>>>,
    host: Mutex<String>,
    connected: AtomicBool,
    failure: Mutex<Option<String>>,
    watches: Mutex<HashSet<String>>,
}

impl Direct {
    pub(crate) fn new(endpoint: &str, instance_id: &str, token: &str) -> Arc<Self> {
        Arc::new(Self {
            endpoint: endpoint.into(),
            instance_id: instance_id.into(),
            token: token.into(),
            connection: tokio::sync::Mutex::new(None),
            host: Mutex::new(String::new()),
            connected: AtomicBool::new(false),
            failure: Mutex::new(None),
            watches: Mutex::new(HashSet::new()),
        })
    }

    async fn client(&self) -> Result<Arc<RpcClient>> {
        let mut slot = self.connection.lock().await;
        if let Some(rpc) = &*slot {
            return Ok(rpc.clone());
        }
        let endpoint = endpoint_url(&self.endpoint)?;
        let rpc = Arc::new(
            zeron_rpc::connect_ws_authorized(&endpoint, Some(&format!("Bearer {}", self.token)))
                .await
                .map_err(rpc_error)?,
        );
        let hello = timed_call(&rpc, "DirectHello", json!({})).await?;
        if hello["instanceId"].as_str() != Some(&self.instance_id)
            || hello["protocol"].as_u64() != Some(1)
        {
            return Err(ClientError::Auth(
                "This address belongs to a different instance; pair it again".into(),
            ));
        }
        *lock(&self.host) = hello["deviceId"].as_str().unwrap_or_default().to_owned();
        *slot = Some(rpc.clone());
        Ok(rpc)
    }

    pub(crate) async fn call_for(
        &self,
        device: &str,
        method: &str,
        params: Value,
    ) -> Result<Value> {
        let rpc = self.client().await?;
        if device != *lock(&self.host) {
            return Err(ClientError::Unsupported(
                "This connection controls one instance".into(),
            ));
        }
        timed_call(&rpc, method, params).await
    }

    pub(crate) fn posture(&self) -> crate::live::RegistryPosture {
        let connected = self.connected.load(Ordering::Acquire);
        crate::live::RegistryPosture {
            connected,
            synced: connected,
            retry_at_ms: None,
            last_failure: lock(&self.failure).clone(),
        }
    }

    pub(crate) fn start(self: &Arc<Self>, inner: &Arc<ClientInner>) {
        let direct = self.clone();
        let weak = Arc::downgrade(inner);
        let cancel = inner.cancel.clone();
        crate::runtime::shared().spawn(async move {
            loop {
                let Some(inner) = weak.upgrade() else { return };
                let outcome = tokio::select! { _ = cancel.cancelled() => return, result = direct.synchronize(&inner) => result };
                match outcome {
                    Ok(()) => { direct.connected.store(true, Ordering::Release); *lock(&direct.failure) = None; }
                    Err(err) => {
                        direct.connected.store(false, Ordering::Release);
                        *lock(&direct.failure) = Some(err.to_string());
                        *direct.connection.lock().await = None;
                        inner.workspace.replace_presence(Default::default());
                        inner.workspace.mutate(|doc| doc.mark_disconnected());
                    }
                }
                inner.recompute_connectivity();
                inner.recompute_workspace();
                let wake = inner.live().unwrap().delivery_wake.clone();
                drop(inner);
                tokio::select! { _ = cancel.cancelled() => return, _ = wake.notified() => {}, _ = tokio::time::sleep(Duration::from_secs(1)) => {} }
            }
        });
    }

    async fn synchronize(self: &Arc<Self>, inner: &Arc<ClientInner>) -> Result<()> {
        let rpc = self.client().await?;
        let live = inner.live().ok_or(ClientError::Closed)?;
        let reply = timed_call(&rpc, methods::DIRECT_REGISTRY, json!({})).await?;
        let rows = serde_json::from_value(reply["rows"].clone())
            .map_err(|e| ClientError::HostError(format!("Invalid workspace: {e}")))?;
        inner.workspace.mutate(|doc| doc.replace_direct_rows(rows));
        let batches = inner.workspace.mutate(|doc| doc.take_pushable());
        for batch in batches {
            let result = timed_call(
                &rpc,
                methods::DIRECT_REGISTRY_PUSH,
                json!({"ops": batch.ops}),
            )
            .await?;
            let rows = serde_json::from_value(result["rows"].clone())
                .map_err(|e| ClientError::HostError(e.to_string()))?;
            inner.workspace.mutate(|doc| {
                doc.ack_batch(&batch.batch, 0);
                doc.replace_direct_rows(rows);
            });
        }
        live.persist_registry(inner)?;
        inner.synced.store(true, Ordering::Release);
        inner
            .workspace
            .set_presence(&lock(&self.host), crate::now_ms());
        inner.recompute_workspace();
        // Includes cold chats with an outbox restored after process termination.
        let mut after = String::new();
        loop {
            let pending = live
                .store
                .pending_viewer_deliveries(&after, 64)
                .map_err(|e| ClientError::Storage(e.to_string()))?;
            if pending.is_empty() {
                break;
            }
            for chat in pending {
                after = chat.clone();
                if inner.workspace.chat(&chat).is_none() {
                    continue;
                }
                let _session = inner.resume_delivery(&chat)?;
                let version = live
                    .store
                    .sync_job_version(&chat, crate::live::delivery::JOB)
                    .map_err(|e| ClientError::Storage(e.to_string()))?;
                for (id, bytes) in live
                    .store
                    .pending_chat_updates(&chat)
                    .map_err(|e| ClientError::Storage(e.to_string()))?
                {
                    timed_call(
                        &rpc,
                        methods::DIRECT_SESSION_PUSH,
                        json!({"chatId": chat, "update": bytes}),
                    )
                    .await?;
                    live.store
                        .acknowledge_chat_update(&chat, &id)
                        .map_err(|e| ClientError::Storage(e.to_string()))?;
                }
                if let Some(version) = version {
                    live.store
                        .complete_sync_job(&chat, crate::live::delivery::JOB, version)
                        .map_err(|e| ClientError::Storage(e.to_string()))?;
                }
            }
        }
        Ok(())
    }

    pub(crate) fn watch_session(
        self: &Arc<Self>,
        inner: &Arc<ClientInner>,
        core: &Arc<SessionCore>,
    ) {
        let chat = core.chat_id.clone();
        if !lock(&self.watches).insert(chat.clone()) {
            return;
        }
        let direct = self.clone();
        let weak = Arc::downgrade(core);
        let account = Arc::downgrade(inner);
        let cancel = inner.cancel.clone();
        crate::runtime::shared().spawn(async move {
            loop {
                let result = tokio::select! { _ = cancel.cancelled() => break, result = direct.stream_session(&account, &weak, &chat) => result };
                if weak.upgrade().is_none() { break; }
                if let Err(err) = result { tracing::debug!(%err, "direct session reconnect"); }
                tokio::select! { _ = cancel.cancelled() => break, _ = tokio::time::sleep(Duration::from_secs(1)) => {} }
            }
            lock(&direct.watches).remove(&chat);
        });
    }

    async fn stream_session(
        &self,
        account: &Weak<ClientInner>,
        weak: &Weak<SessionCore>,
        chat: &str,
    ) -> Result<()> {
        let rpc = self.client().await?;
        let mut stream = rpc
            .subscribe_scoped(methods::DIRECT_SESSION, json!({"chatId": chat}))
            .await
            .map_err(rpc_error)?;
        let mut last_save = std::time::Instant::now() - Duration::from_secs(2);
        loop {
            let frame = tokio::select! {
                frame = stream.recv() => frame.ok_or(ClientError::Closed)?,
                _ = tokio::time::sleep(Duration::from_secs(2)) => {
                    if weak.upgrade().is_none() { return Ok(()); }
                    let active = self.connection.lock().await;
                    if active.as_ref().is_none_or(|c| !Arc::ptr_eq(c, &rpc)) { return Err(ClientError::Closed); }
                    continue;
                }
            };
            let Some(core) = weak.upgrade() else {
                return Ok(());
            };
            let bytes: Vec<u8> = serde_json::from_value(frame["update"].clone())
                .map_err(|e| ClientError::HostError(e.to_string()))?;
            core.doc()
                .doc()
                .import(&bytes)
                .map_err(|e| ClientError::HostError(e.to_string()))?;
            core.set_hydrated();
            core.schedule_refresh();
            if last_save.elapsed() >= Duration::from_secs(1) {
                if let Some(inner) = account.upgrade() {
                    Self::persist_session(&inner, &core)?;
                }
                last_save = std::time::Instant::now();
            }
        }
    }

    pub(crate) fn persist_session(inner: &ClientInner, core: &SessionCore) -> Result<()> {
        let live = inner.live().ok_or(ClientError::Closed)?;
        let bytes = core.doc().export_snapshot()?;
        live.store
            .save_snapshot_with_cursor(&core.chat_id, &bytes, 0, 2)
            .map_err(|e| ClientError::Storage(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn endpoint_security_boundary() {
        assert_eq!(
            endpoint_url("https://host.example").unwrap(),
            "wss://host.example/rpc"
        );
        assert_eq!(
            endpoint_url("http://127.0.0.1:1234").unwrap(),
            "ws://127.0.0.1:1234/rpc"
        );
        for url in [
            "http://192.168.1.1",
            "ws://public.example",
            "https://user:pass@host",
            "https://host?token=secret",
            "https://host#secret",
            "file:///tmp/a",
        ] {
            assert!(endpoint_url(url).is_err(), "{url}");
        }
    }
}
