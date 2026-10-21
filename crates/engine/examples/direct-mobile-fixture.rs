//! Isolated simulator fixture; never connects to a user's engine or agent.
use std::sync::Arc;
use zeron_engine::{EngineCore, HarnessRegistry};
use zeron_harness::mock::MockHarness;
use zeron_proto::{AgentEvent, DoneStatus, HarnessId};
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(MockHarness {
        script: vec![
            AgentEvent::TextDelta {
                text: "Hello from your direct instance.".into(),
            },
            AgentEvent::Done {
                status: DoneStatus::Completed,
                result: None,
                error: None,
                session_id: Some("direct-fixture".into()),
            },
        ],
    }));
    let core = EngineCore::assemble(root.path(), Arc::new(registry), HarnessId::Mock, None)?;
    let ipc = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let ipc_url = format!("ws://{}", ipc.local_addr()?);
    tokio::spawn(zeron_rpc::serve_ws_listener(ipc, core.rpc_service()));
    let store = zeron_rpc::remote::RemoteStore::open(root.path().join("gateway"))?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let url = format!("http://{}", listener.local_addr()?);
    let key = store.pairing_key()?;
    let output = std::env::args().nth(1).expect("output env file");
    std::fs::write(
        output,
        format!("TEST_RUNNER_ZERON_DIRECT_ENDPOINT={url}\nTEST_RUNNER_ZERON_DIRECT_KEY={key}\n"),
    )?;
    zeron_rpc::remote::serve(listener, ipc_url, store).await
}
