//! MCP -> real engine RPC -> desktop broker -> reply, with synthetic tabs.
use serde_json::json;
use std::sync::Arc;
use zeron_engine::{EngineCore, HarnessRegistry};
use zeron_mcp::{Origin, Tools, Zeron};
use zeron_rpc::{memory_client, methods};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn browser_mcp_routes_through_the_running_engine_and_honors_revocation() {
    let temp = tempfile::tempdir().unwrap();
    let core = EngineCore::assemble(
        temp.path(),
        Arc::new(HarnessRegistry::new()),
        zeron_proto::HarnessId::Mock,
        None,
    )
    .unwrap();
    let desktop = memory_client(core.rpc_service());
    let mut stream = desktop
        .subscribe_scoped(methods::ATTACH_BROWSER_HOST, json!({"hostId":"desktop"}))
        .await
        .unwrap();
    assert_eq!(stream.recv().await.unwrap()["ready"], true);
    let tabs = json!([{"id":"desktop/1","chatId":"origin","title":"Test page","url":"https://example.test/","loading":false,"enabled":true}]);
    desktop
        .call(
            methods::UPDATE_BROWSER_HOST,
            json!({"hostId":"desktop","revision":1,"tabs":tabs}),
        )
        .await
        .unwrap();
    let tools = Arc::new(Tools::new(Arc::new(Zeron::with_client(
        memory_client(core.rpc_service()),
        Origin {
            chat_id: Some("origin".into()),
            device_id: Some(core.device_id.clone()),
            side_chats_only: false,
        },
    ))));
    assert_eq!(
        tools.call("browser_list_tabs", json!({})).await.unwrap()[0]["id"],
        "desktop/1"
    );
    let operation = tokio::spawn({
        let tools = tools.clone();
        async move {
            tools
                .call("browser_snapshot", json!({"browserId":"desktop/1"}))
                .await
        }
    });
    let request = stream.recv().await.unwrap();
    assert_eq!(request["request"]["action"], "snapshot");
    desktop.call(methods::REPLY_BROWSER_HOST, json!({"hostId":"desktop","requestId":request["requestId"],"result":{"Ok":{"title":"Inspected"}}})).await.unwrap();
    assert_eq!(operation.await.unwrap().unwrap()["title"], "Inspected");
    let mut disabled = tabs;
    disabled[0]["enabled"] = false.into();
    desktop
        .call(
            methods::UPDATE_BROWSER_HOST,
            json!({"hostId":"desktop","revision":2,"tabs":disabled}),
        )
        .await
        .unwrap();
    assert!(
        tools
            .call(
                "browser_click",
                json!({"browserId":"desktop/1","ref":"old"})
            )
            .await
            .unwrap_err()
            .contains("browser_unavailable")
    );
    drop(stream);
    core.shutdown().await;
}
