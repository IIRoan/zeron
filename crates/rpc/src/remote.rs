//! Opt-in gateway for one trusted user's paired devices. Local IPC is never
//! rebound. Put this loopback listener behind an HTTPS private-network proxy.
use crate::{RpcClient, RpcError, RpcReply, RpcService, methods};
use futures::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::{
    Message,
    handshake::server::{ErrorResponse, Request, Response},
};

#[derive(Clone)]
pub struct RemoteStore {
    root: PathBuf,
}
#[derive(Serialize, Deserialize)]
struct Pairing {
    expires: u64,
}
#[derive(Serialize, Deserialize)]
pub struct Device {
    pub id: String,
    pub name: String,
}
#[derive(Serialize, Deserialize)]
struct Identity {
    id: String,
    scope: Value,
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn secret() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}
fn hash(key: &str) -> String {
    format!("{:x}", Sha256::digest(key.as_bytes()))
}
fn denied(reason: &str) -> RpcError {
    RpcError::Failed(reason.into())
}

fn private_write(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

impl RemoteStore {
    pub fn open(root: PathBuf) -> anyhow::Result<Self> {
        for path in [&root, &root.join("pairs"), &root.join("devices")] {
            std::fs::create_dir_all(path)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
            }
        }
        Ok(Self { root })
    }
    pub fn pairing_key(&self) -> anyhow::Result<String> {
        let code = secret();
        private_write(
            &self.root.join("pairs").join(hash(&code)),
            &serde_json::to_vec(&Pairing {
                expires: now() + 600,
            })?,
        )?;
        Ok(code)
    }
    fn check_pair(&self, code: &str) -> bool {
        std::fs::read(self.root.join("pairs").join(hash(code)))
            .ok()
            .and_then(|b| serde_json::from_slice::<Pairing>(&b).ok())
            .is_some_and(|p| p.expires > now())
    }
    fn redeem(&self, code: &str, name: &str) -> Result<(Device, String), RpcError> {
        if !self.check_pair(code) {
            return Err(denied("Pairing key expired or already used"));
        }
        let source = self.root.join("pairs").join(hash(code));
        // Rename is the cross-connection, cross-process one-time claim.
        let claimed = self
            .root
            .join("pairs")
            .join(format!("used-{}", uuid::Uuid::new_v4()));
        std::fs::rename(source, &claimed).map_err(|_| denied("Pairing key already used"))?;
        let _ = std::fs::remove_file(claimed);
        let token = secret();
        let device = Device {
            id: hash(&token),
            name: name.chars().take(80).collect(),
        };
        private_write(
            &self.root.join("devices").join(&device.id),
            &serde_json::to_vec(&device).map_err(|_| denied("Cannot store device"))?,
        )
        .map_err(|_| denied("Cannot store device"))?;
        Ok((device, token))
    }
    fn authorized(&self, token: &str) -> bool {
        self.device(&hash(token)).is_some()
    }
    fn device(&self, id: &str) -> Option<Device> {
        if id.len() != 64 || !id.bytes().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        serde_json::from_slice(&std::fs::read(self.root.join("devices").join(id)).ok()?).ok()
    }
    pub fn devices(&self) -> anyhow::Result<Vec<Device>> {
        Ok(std::fs::read_dir(self.root.join("devices"))?
            .filter_map(|e| self.device(&e.ok()?.file_name().to_string_lossy()))
            .collect())
    }
    pub fn revoke(&self, id: &str) -> anyhow::Result<()> {
        anyhow::ensure!(self.device(id).is_some(), "Unknown device");
        std::fs::remove_file(self.root.join("devices").join(id))?;
        Ok(())
    }
    fn identity(&self, scope: Value) -> anyhow::Result<String> {
        let path = self.root.join("identity.json");
        if path.exists() {
            let identity: Identity = serde_json::from_slice(&std::fs::read(path)?)?;
            anyhow::ensure!(
                identity.scope == scope,
                "The engine identity/profile changed. Use a separate gateway data directory and pair again."
            );
            return Ok(identity.id);
        }
        let identity = Identity {
            id: uuid::Uuid::new_v4().to_string(),
            scope,
        };
        private_write(&path, &serde_json::to_vec(&identity)?)?;
        Ok(identity.id)
    }
}

struct Gateway {
    engine: Arc<RpcClient>,
    store: RemoteStore,
    instance: String,
    device: String,
    credential: Credential,
}
#[derive(Clone)]
enum Credential {
    Pair(String),
    Device(String),
}

fn permitted(method: &str) -> bool {
    matches!(
        method,
        methods::DIRECT_REGISTRY
            | methods::DIRECT_REGISTRY_PUSH
            | methods::DIRECT_SESSION
            | methods::DIRECT_SESSION_PUSH
            | methods::LIST_HARNESSES
            | methods::LIST_MODELS
            | methods::LIST_REFS
            | methods::LIST_FOLDERS
            | methods::SEARCH_FILES
            | methods::CREATE_WORKTREE
            | methods::SWITCH_REF
            | methods::READ_ATTACHMENT_CHUNK
            | methods::UPLOAD_CHUNK
            | methods::UPLOAD_COMMIT
            | methods::BEGIN_QUEUED_MESSAGE_EDIT
            | methods::RENEW_QUEUED_MESSAGE_EDIT
            | methods::FINISH_QUEUED_MESSAGE_EDIT
            | methods::UPDATE_QUEUED_MESSAGE
            | methods::MOVE_QUEUED_MESSAGE
            | methods::REMOVE_QUEUED_MESSAGE
            | methods::SEND_QUEUED_MESSAGE_NOW
            | methods::STEER_QUEUED_MESSAGE_NOW
            | methods::WATCH_CHECKOUT_CHANGE_REQUEST
            | methods::LIST_WORKSPACE_DIRECTORY
            | methods::READ_WORKSPACE_FILE
    )
}

#[async_trait::async_trait]
impl RpcService for Gateway {
    async fn handle(&self, method: &str, params: Value) -> Result<RpcReply, RpcError> {
        match &self.credential {
            Credential::Pair(code) => {
                if method != "Pair" {
                    return Err(denied("Pairing connections cannot control the instance"));
                }
                let (device, token) = self
                    .store
                    .redeem(code, params["name"].as_str().unwrap_or("Phone"))?;
                return RpcReply::value(
                    &json!({"instanceId": self.instance, "deviceId": self.device, "credentialId": device.id, "token": token}),
                );
            }
            Credential::Device(token) if !self.store.authorized(token) => {
                return Err(denied("Device revoked"));
            }
            _ => {}
        }
        if method == "DirectHello" {
            return RpcReply::value(
                &json!({"protocol": 1, "instanceId": self.instance, "deviceId": self.device}),
            );
        }
        if !permitted(method)
            || params
                .get("targetDeviceId")
                .is_some_and(|id| id.as_str() != Some(&self.device))
        {
            return Err(denied(
                "Method is not available through this direct connection",
            ));
        }
        if matches!(
            method,
            methods::DIRECT_SESSION | methods::WATCH_CHECKOUT_CHANGE_REQUEST
        ) {
            let stream = self.engine.subscribe_scoped(method, params).await?;
            Ok(RpcReply::Stream(
                futures::stream::unfold(stream, |mut s| async move { Some((s.recv().await?, s)) })
                    .boxed(),
            ))
        } else {
            tokio::time::timeout(Duration::from_secs(180), self.engine.call(method, params))
                .await
                .map_err(|_| denied("Engine request timed out"))?
                .map(RpcReply::Value)
        }
    }
}

pub async fn serve(listener: TcpListener, ipc: String, store: RemoteStore) -> anyhow::Result<()> {
    let engine = crate::connect_ws(&ipc).await?;
    let info = engine.call(methods::ENGINE_INFO, json!({})).await?;
    let scope = json!({"deviceId": info["deviceId"], "workspaceScope": info["workspaceScope"]});
    let instance = store.identity(scope)?;
    engine
        .call(methods::DIRECT_REGISTRY, json!({}))
        .await
        .map_err(|_| {
            anyhow::anyhow!(
                "Update the running Zeron engine to a build with direct instance support"
            )
        })?;
    let device = info["deviceId"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("Engine did not provide its identity"))?
        .to_owned();
    let limit = Arc::new(tokio::sync::Semaphore::new(32));
    let mut connections = tokio::task::JoinSet::new();
    loop {
        let accepted = tokio::select! {
            accepted = listener.accept() => accepted?,
            _ = connections.join_next(), if !connections.is_empty() => continue,
        };
        let (socket, _) = accepted;
        let Ok(permit) = limit.clone().try_acquire_owned() else {
            continue;
        };
        let (ipc, store, instance, device, info) = (
            ipc.clone(),
            store.clone(),
            instance.clone(),
            device.clone(),
            info.clone(),
        );
        connections.spawn(async move {
            let _permit = permit;
            let mut credential = None;
            let check = |request: &Request, response: Response| -> Result<Response, ErrorResponse> {
                let auth = request.headers().get("authorization").and_then(|v| v.to_str().ok()).unwrap_or("");
                credential = if let Some(key) = auth.strip_prefix("Pair ").filter(|k| k.len() == 64 && store.check_pair(k)) {
                    Some(Credential::Pair(key.into()))
                } else { auth.strip_prefix("Bearer ").filter(|k| k.len() == 64 && store.authorized(k)).map(|k| Credential::Device(k.into())) };
                if request.uri().path() != "/rpc" || request.uri().query().is_some() || request.headers().contains_key("origin") || credential.is_none() {
                    let mut denied = ErrorResponse::new(Some("Unauthorized".into()));
                    *denied.status_mut() = tokio_tungstenite::tungstenite::http::StatusCode::UNAUTHORIZED;
                    return Err(denied);
                }
                Ok(response)
            };
            let config = tokio_tungstenite::tungstenite::protocol::WebSocketConfig {
                max_message_size: Some(64 * 1024 * 1024), max_frame_size: Some(64 * 1024 * 1024), ..Default::default()
            };
            let Ok(Ok(socket)) = tokio::time::timeout(Duration::from_secs(5), tokio_tungstenite::accept_hdr_async_with_config(socket, check, Some(config))).await else { return };
            let Some(credential) = credential else { return };
            let Ok(engine) = crate::connect_ws(&ipc).await else { return };
            if engine.call(methods::ENGINE_INFO, json!({})).await.ok().as_ref() != Some(&info) { return; }
            let engine = Arc::new(engine);
            let service = Arc::new(Gateway { engine, store: store.clone(), instance, device, credential: credential.clone() });
            let (out_tx, mut out_rx) = tokio::sync::mpsc::channel::<String>(64);
            let (in_tx, in_rx) = tokio::sync::mpsc::channel::<String>(64);
            let task = tokio::spawn(crate::serve_connection(service, out_tx, in_rx));
            let (mut sink, mut source) = socket.split();
            let mut check_revocation = tokio::time::interval(Duration::from_secs(5));
            loop {
                tokio::select! {
                    _ = check_revocation.tick() => {
                        if let Credential::Device(token) = &credential && !store.authorized(token) { break; }
                    }
                    message = source.next() => match message {
                        Some(Ok(Message::Text(text))) => if in_tx.send(text).await.is_err() { break; },
                        Some(Ok(Message::Ping(bytes))) => if sink.send(Message::Pong(bytes)).await.is_err() { break; },
                        Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                        _ => {}
                    },
                    text = out_rx.recv() => match text {
                        Some(text) => if sink.send(Message::Text(text)).await.is_err() { break; },
                        None => break,
                    }
                }
            }
            drop(in_tx);
            let _ = sink.close().await;
            let _ = task.await;
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pairing_is_single_use_expires_and_revokes() {
        let dir = tempfile::tempdir().unwrap();
        let store = RemoteStore::open(dir.path().into()).unwrap();
        let code = store.pairing_key().unwrap();
        assert!(store.check_pair(&code));
        let (device, token) = store.redeem(&code, "iPhone").unwrap();
        assert!(store.authorized(&token));
        assert!(!store.authorized(&code));
        assert!(store.redeem(&code, "again").is_err());
        store.revoke(&device.id).unwrap();
        assert!(!store.authorized(&token));
        let expired = store.pairing_key().unwrap();
        std::fs::write(
            store.root.join("pairs").join(hash(&expired)),
            serde_json::to_vec(&Pairing { expires: 1 }).unwrap(),
        )
        .unwrap();
        assert!(store.redeem(&expired, "expired").is_err());
    }

    #[test]
    fn scope_change_requires_new_pairing() {
        let dir = tempfile::tempdir().unwrap();
        let store = RemoteStore::open(dir.path().into()).unwrap();
        let id = store
            .identity(json!({"deviceId": "a", "workspaceScope": "local"}))
            .unwrap();
        assert_eq!(
            id,
            store
                .identity(json!({"deviceId": "a", "workspaceScope": "local"}))
                .unwrap()
        );
        assert!(
            store
                .identity(json!({"deviceId": "b", "workspaceScope": "local"}))
                .is_err()
        );
    }

    struct FakeEngine;
    #[async_trait::async_trait]
    impl RpcService for FakeEngine {
        async fn handle(&self, method: &str, _: Value) -> Result<RpcReply, RpcError> {
            match method {
                methods::ENGINE_INFO => {
                    RpcReply::value(&json!({"deviceId":"host", "workspaceScope":"local"}))
                }
                methods::DIRECT_REGISTRY => RpcReply::value(&json!({"rows":[]})),
                methods::LIST_MODELS => RpcReply::value(&json!([])),
                methods::DIRECT_SESSION => Ok(RpcReply::Stream(
                    futures::stream::once(async { json!({"ready":true}) })
                        .chain(futures::stream::pending())
                        .boxed(),
                )),
                _ => panic!("Gateway forwarded an unauthorized method: {method}"),
            }
        }
    }

    #[tokio::test]
    async fn authentication_precedes_rpc_and_revocation_closes_active_streams() {
        let dir = tempfile::tempdir().unwrap();
        let store = RemoteStore::open(dir.path().into()).unwrap();
        let ipc = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let ipc_url = format!("ws://{}", ipc.local_addr().unwrap());
        let ipc_task = tokio::spawn(crate::serve_ws_listener(ipc, Arc::new(FakeEngine)));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}/rpc", listener.local_addr().unwrap());
        let server = tokio::spawn(serve(listener, ipc_url, store.clone()));
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(crate::connect_ws(&url).await.is_err());
        assert!(
            crate::connect_ws_authorized(&url, Some("Bearer bad"))
                .await
                .is_err()
        );
        let code = store.pairing_key().unwrap();
        let pair = crate::connect_ws_authorized(&url, Some(&format!("Pair {code}")))
            .await
            .unwrap();
        assert!(pair.call(methods::LIST_MODELS, json!({})).await.is_err());
        let paired = pair.call("Pair", json!({"name":"Phone"})).await.unwrap();
        assert!(pair.call("Pair", json!({})).await.is_err());
        let token = paired["token"].as_str().unwrap();
        let rpc = crate::connect_ws_authorized(&url, Some(&format!("Bearer {token}")))
            .await
            .unwrap();
        assert!(rpc.call("DirectHello", json!({})).await.is_ok());
        assert!(rpc.call(methods::STOP_ENGINE, json!({})).await.is_err());
        assert!(
            rpc.call(methods::LIST_MODELS, json!({"targetDeviceId":"other"}))
                .await
                .is_err()
        );
        assert!(rpc.call(methods::LIST_MODELS, json!({})).await.is_ok());
        let mut stream = rpc
            .subscribe_scoped(methods::DIRECT_SESSION, json!({}))
            .await
            .unwrap();
        assert_eq!(stream.recv().await.unwrap()["ready"], true);
        // Even valid credentials never permit browser-origin or query-token dials.
        use tokio_tungstenite::tungstenite::client::IntoClientRequest;
        let mut request = url.clone().into_client_request().unwrap();
        request
            .headers_mut()
            .insert("authorization", format!("Bearer {token}").parse().unwrap());
        request
            .headers_mut()
            .insert("origin", "https://untrusted.example".parse().unwrap());
        assert!(tokio_tungstenite::connect_async(request).await.is_err());
        store
            .revoke(paired["credentialId"].as_str().unwrap())
            .unwrap();
        assert!(rpc.call(methods::LIST_MODELS, json!({})).await.is_err());
        assert!(
            tokio::time::timeout(Duration::from_secs(7), stream.recv())
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            crate::connect_ws_authorized(&url, Some(&format!("Bearer {token}")))
                .await
                .is_err()
        );
        server.abort();
        ipc_task.abort();
    }
}
