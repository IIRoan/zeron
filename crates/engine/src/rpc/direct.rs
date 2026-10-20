//! Direct, authenticated single-instance peers. The gateway authenticates before
//! forwarding; these methods are also available over the existing local IPC.
use super::*;
use serde_json::{Value, json};
use std::sync::Arc;

pub(super) fn handles(method: &str) -> bool {
    matches!(
        method,
        methods::DIRECT_REGISTRY
            | methods::DIRECT_REGISTRY_PUSH
            | methods::DIRECT_SESSION
            | methods::DIRECT_SESSION_PUSH
    )
}

impl EngineRpc {
    pub(super) async fn direct_rpc(
        &self,
        method: &str,
        params: Value,
    ) -> Result<RpcReply, RpcError> {
        let failed = |e: String| RpcError::Failed(e);
        match method {
            methods::DIRECT_REGISTRY => RpcReply::value(&json!({
                "protocol": 1, "deviceId": self.doc_host.device_id(),
                "rows": self.workspace.direct_rows(),
            })),
            methods::DIRECT_REGISTRY_PUSH => {
                let ops: Vec<zeron_doc::registry::RowOp> =
                    serde_json::from_value(params["ops"].clone())
                        .map_err(|e| RpcError::BadParams(e.to_string()))?;
                if ops.len() > 400
                    || ops.iter().any(|op| {
                        !matches!(
                            op.kind.as_str(),
                            "chats" | "spaces" | "preferences" | "sidebarPins" | "sessions"
                        )
                    })
                {
                    return Err(RpcError::BadParams("invalid direct registry batch".into()));
                }
                // A direct connection controls this instance only; never publish a
                // new owner or rewrite device identity through the viewer registry.
                for op in &ops {
                    if let Some(device) = op.set.as_ref().and_then(|s| s.get("deviceId"))
                        && device.as_str() != Some(self.doc_host.device_id())
                    {
                        return Err(RpcError::BadParams(
                            "foreign device in direct registry write".into(),
                        ));
                    }
                    if op.kind == "chats"
                        && let Some(chat) = self
                            .workspace
                            .chat(&op.id)
                            .map_err(|e| failed(e.to_string()))?
                        && chat.device_id != self.doc_host.device_id()
                    {
                        return Err(RpcError::BadParams(
                            "chat belongs to another instance".into(),
                        ));
                    }
                }
                self.workspace
                    .direct_push(ops)
                    .map_err(|e| failed(e.to_string()))?;
                RpcReply::value(&json!({"rows": self.workspace.direct_rows()}))
            }
            methods::DIRECT_SESSION | methods::DIRECT_SESSION_PUSH => {
                let p: ChatParams = parse_params(params.clone())?;
                let chat = self
                    .workspace
                    .chat(&p.chat_id)
                    .map_err(|e| failed(e.to_string()))?
                    .ok_or_else(|| RpcError::BadParams("unknown chat".into()))?;
                if chat.device_id != self.doc_host.device_id() {
                    return Err(RpcError::BadParams(
                        "chat belongs to another instance".into(),
                    ));
                }
                let handle = self
                    .doc_host
                    .open(&p.chat_id)
                    .map_err(|e| failed(e.to_string()))?;
                if method == methods::DIRECT_SESSION_PUSH {
                    let bytes: Vec<u8> = serde_json::from_value(params["update"].clone())
                        .map_err(|e| RpcError::BadParams(e.to_string()))?;
                    if bytes.len() > 8 * 1024 * 1024 {
                        return Err(RpcError::BadParams("session update exceeds 8 MiB".into()));
                    }
                    handle
                        .doc()
                        .doc()
                        .import(&bytes)
                        .map_err(|e| failed(e.to_string()))?;
                    // ACK only after the outbox is durable; replaying a CRDT update
                    // cannot execute a command twice (host processed-command ledger).
                    self.doc_host
                        .persist_fork(&handle)
                        .map_err(|e| failed(e.to_string()))?;
                    return RpcReply::value(&json!({"ok": true}));
                }
                let notify = Arc::new(tokio::sync::Notify::new());
                let wake = notify.clone();
                let subscription = handle.doc().doc().subscribe_root(Arc::new(move |_| {
                    wake.notify_one();
                }));
                let stream = futures::stream::unfold(
                    (handle, notify, subscription, None::<loro::VersionVector>),
                    |(handle, notify, subscription, previous)| async move {
                        if previous.is_some() {
                            notify.notified().await;
                        }
                        let doc = handle.doc().doc();
                        // Capture the frontier before export. An intervening commit
                        // may be sent twice, which Loro deduplicates, never skipped.
                        let vv = doc.oplog_vv();
                        let bytes = match previous {
                            Some(ref vv) => doc.export(loro::ExportMode::updates(vv)),
                            None => doc.export(loro::ExportMode::Snapshot),
                        }
                        .ok()?;
                        let frame = json!({"update": bytes});
                        Some((frame, (handle, notify, subscription, Some(vv))))
                    },
                );
                Ok(RpcReply::Stream(stream.boxed()))
            }
            _ => Err(RpcError::UnknownMethod(method.into())),
        }
    }
}
