//! Real engine + authenticated gateway + Rust mobile client, without an edge.
use serde_json::json;
use std::{sync::Arc, time::Duration};
use zeron_client::{Client, ClientConfig, NewSession, SendRequest, SessionTarget};
use zeron_engine::{EngineCore, HarnessRegistry};
use zeron_harness::mock::MockHarness;
use zeron_proto::{AgentEvent, DoneStatus, HarnessId};

async fn until(label: &str, mut predicate: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(20), async {
        while !predicate() {
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out: {label}"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn direct_mobile_pair_create_send_stream_restart_and_revoke() {
    let dir = tempfile::tempdir().unwrap();
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(MockHarness {
        script: vec![
            AgentEvent::TextDelta {
                text: "reply from the instance".into(),
            },
            AgentEvent::Done {
                status: DoneStatus::Completed,
                result: None,
                error: None,
                session_id: Some("mock-direct".into()),
            },
        ],
    }));
    let core = EngineCore::assemble(
        &dir.path().join("engine"),
        Arc::new(registry),
        HarnessId::Mock,
        None,
    )
    .unwrap();
    let ipc = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ipc_url = format!("ws://{}", ipc.local_addr().unwrap());
    let ipc_task = tokio::spawn(zeron_rpc::serve_ws_listener(ipc, core.rpc_service()));
    let store = zeron_rpc::remote::RemoteStore::open(dir.path().join("gateway")).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let url = format!("http://{address}");
    let gateway = tokio::spawn(zeron_rpc::remote::serve(
        listener,
        ipc_url.clone(),
        store.clone(),
    ));
    tokio::time::sleep(Duration::from_millis(100)).await;
    let credentials = zeron_client::direct::pair(&url, &store.pairing_key().unwrap(), "Test phone")
        .await
        .unwrap();
    let config = ClientConfig::new(&url, dir.path().join("phone"));
    let phone = Client::new(
        config.clone(),
        credentials.clone(),
        Arc::new(zeron_client::events::NullListener),
    )
    .unwrap();
    until("host discovered", || {
        phone
            .workspace()
            .devices
            .iter()
            .any(|d| d.id == core.device_id)
    })
    .await;
    let chat = phone
        .create_session(NewSession {
            target: SessionTarget::Projectless {
                device_id: core.device_id.clone(),
            },
            config: None,
            branch: None,
            cwd: None,
            title: Some("Direct test".into()),
        })
        .unwrap();
    let handle = phone.open_session(&chat).unwrap();
    handle.send(SendRequest::text("hello instance")).unwrap();
    until("reply on phone", || handle.snapshot().entries.iter().any(|entry| entry.parts().iter().any(|part| matches!(part, zeron_doc::MessagePart::Text { text, .. } if text.contains("reply from the instance"))))).await;
    until("send acknowledged", || {
        handle.composer().pending_sends.is_empty()
    })
    .await;
    let host_doc = core.doc_host.open(&chat).unwrap();
    assert_eq!(host_doc.doc().read_commands().unwrap().len(), 1);
    let replay = host_doc.doc().export_snapshot().unwrap();
    let local = zeron_rpc::memory_client(core.rpc_service());
    local
        .call(
            zeron_rpc::methods::DIRECT_SESSION_PUSH,
            json!({"chatId":chat,"update":replay}),
        )
        .await
        .unwrap();
    assert_eq!(
        host_doc.doc().read_commands().unwrap().len(),
        1,
        "replayed delivery must not mint another command"
    );
    phone.on_background();
    phone.shutdown();
    drop(handle);
    drop(phone);
    let phone = Client::new(
        config,
        credentials,
        Arc::new(zeron_client::events::NullListener),
    )
    .unwrap();
    until("cached chat restored", || {
        phone.workspace().session(&chat).is_some()
    })
    .await;
    let handle = phone.open_session(&chat).unwrap();
    until("cached transcript", || {
        !handle.snapshot().entries.is_empty()
    })
    .await;
    let id = store.devices().unwrap().remove(0).id;
    store.revoke(&id).unwrap();
    until("revoked connection", || {
        !matches!(
            phone.connectivity().state,
            zeron_client::ConnectivityState::Connected
        )
    })
    .await;
    phone.shutdown();
    gateway.abort();
    ipc_task.abort();
    core.shutdown().await;
}

struct ControlledHarness;
#[async_trait::async_trait]
impl zeron_harness::Harness for ControlledHarness {
    fn id(&self) -> HarnessId {
        HarnessId::Mock
    }
    fn display_name(&self) -> &str {
        "Controlled"
    }
    fn supports_steering(&self) -> bool {
        true
    }
    fn steering_mode(&self) -> zeron_proto::SteeringMode {
        zeron_proto::SteeringMode::StepBoundary
    }
    fn reasoning_levels(&self) -> &[zeron_proto::ReasoningLevel] {
        &[]
    }
    async fn models(&self) -> Result<Vec<zeron_proto::Model>, zeron_harness::HarnessError> {
        Ok(vec![])
    }
    async fn run(
        &self,
        request: zeron_proto::RunRequest,
        controls: zeron_harness::RunControls,
    ) -> Result<
        futures::stream::BoxStream<'static, Result<AgentEvent, zeron_harness::HarnessError>>,
        zeron_harness::HarnessError,
    > {
        use futures::StreamExt;
        let (tx, rx) = tokio::sync::mpsc::channel(8);
        tokio::spawn(async move {
            let _ = tx
                .send(Ok(AgentEvent::TextDelta {
                    text: "started".into(),
                }))
                .await;
            let status = if request.prompt == "question" {
                let answers = (controls.request_input)(vec![zeron_proto::UserInputQuestion {
                    id: "q".into(),
                    header: "Question".into(),
                    question: "Continue?".into(),
                    options: vec!["Yes".into()],
                    prefill: None,
                    multiline: false,
                    multi_select: false,
                }])
                .await
                .unwrap();
                assert_eq!(answers[0].labels, ["Yes"]);
                DoneStatus::Completed
            } else {
                controls.interrupt.cancelled().await;
                DoneStatus::Interrupted
            };
            let _ = tx
                .send(Ok(AgentEvent::Done {
                    status,
                    result: None,
                    error: None,
                    session_id: None,
                }))
                .await;
        });
        Ok(
            futures::stream::unfold(rx, |mut rx| async move { Some((rx.recv().await?, rx)) })
                .boxed(),
        )
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn direct_mobile_answers_questions_and_interrupts_runs() {
    let dir = tempfile::tempdir().unwrap();
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(ControlledHarness));
    let core = EngineCore::assemble(
        &dir.path().join("engine"),
        Arc::new(registry),
        HarnessId::Mock,
        None,
    )
    .unwrap();
    let ipc = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ipc_url = format!("ws://{}", ipc.local_addr().unwrap());
    let ipc_task = tokio::spawn(zeron_rpc::serve_ws_listener(ipc, core.rpc_service()));
    let store = zeron_rpc::remote::RemoteStore::open(dir.path().join("gateway")).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let gateway = tokio::spawn(zeron_rpc::remote::serve(listener, ipc_url, store.clone()));
    tokio::time::sleep(Duration::from_millis(100)).await;
    let credentials = zeron_client::direct::pair(&url, &store.pairing_key().unwrap(), "Phone")
        .await
        .unwrap();
    let phone = Client::new(
        ClientConfig::new(&url, dir.path().join("phone")),
        credentials,
        Arc::new(zeron_client::events::NullListener),
    )
    .unwrap();
    until("host discovered", || {
        phone
            .workspace()
            .devices
            .iter()
            .any(|d| d.id == core.device_id)
    })
    .await;
    let chat = phone
        .create_session(NewSession {
            target: SessionTarget::Projectless {
                device_id: core.device_id.clone(),
            },
            config: None,
            branch: None,
            cwd: None,
            title: None,
        })
        .unwrap();
    let session = phone.open_session(&chat).unwrap();
    session.send(SendRequest::text("question")).unwrap();
    until("input requested", || {
        session.composer().open_input.is_some()
    })
    .await;
    let request = session.composer().open_input.clone().unwrap();
    session
        .respond_input(
            &request.request_id,
            vec![zeron_proto::UserInputAnswer {
                question_id: "q".into(),
                labels: vec!["Yes".into()],
            }],
        )
        .unwrap();
    until("question answered", || {
        session.composer().open_input.is_none()
            && !session.composer().live.turn_running
            && session.composer().pending_sends.is_empty()
    })
    .await;
    session.send(SendRequest::text("wait")).unwrap();
    until("running", || session.composer().live.turn_running).await;
    session.interrupt().unwrap();
    until("interrupted", || {
        !session.composer().live.turn_running && session.composer().pending_sends.is_empty()
    })
    .await;
    phone.shutdown();
    gateway.abort();
    ipc_task.abort();
    core.shutdown().await;
}
