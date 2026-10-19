//! Real Claude stream-json decoding through the engine's document writer.
//! The forked-skill shape is taken from the reported code-review session.

use std::{path::Path, sync::Arc, time::Duration};

use zeron_doc::{MessagePart, MessageRole, SessionMessageEntry, SubagentStatus};
use zeron_engine::{EngineCore, HarnessRegistry};
use zeron_harness::ClaudeHarness;
use zeron_proto::{HarnessId, RunRequest, SandboxLevel, SessionStatus, ToolCall};

const CHAT: &str = "claude-forked-skill";
const CHILD: &str = "claude-forked-skill--sub--skill";

fn assemble(dir: &Path) -> EngineCore {
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../harness/tests/fixtures/fake-claude.sh");
    let registry = Arc::new(HarnessRegistry::new());
    registry.register(Arc::new(ClaudeHarness::new().with_executable(fixture)));
    EngineCore::assemble(dir, registry, HarnessId::ClaudeCode, None).unwrap()
}

fn entries(core: &EngineCore, doc: &str) -> Vec<SessionMessageEntry> {
    core.doc_host
        .open(doc)
        .unwrap()
        .doc()
        .read_entries()
        .unwrap()
}

fn skill(core: &EngineCore) -> Option<MessagePart> {
    entries(core, CHAT)
        .into_iter()
        .flat_map(|entry| entry.parts)
        .find(|part| matches!(part, MessagePart::Tool { id, .. } if id == "skill"))
}

async fn wait_for(mut ready: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while !ready() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("fixture event was persisted");
}

async fn check_forked_skill(parked: bool, failed: bool) {
    let dir = tempfile::tempdir().unwrap();
    let core = assemble(dir.path());
    let request = RunRequest {
        mcp: None,
        prompt: format!(
            "scenario:forked-skill-{}-{}",
            if parked { "parked" } else { "live" },
            if failed { "failed" } else { "success" }
        ),
        harness: None,
        model: None,
        reasoning: None,
        model_options: Default::default(),
        cwd: dir.path().display().to_string(),
        sandbox: SandboxLevel::ReadOnly,
        auto_approve: true,
        attachments: vec![],
        worktree: None,
        resume: None,
    };
    core.sessions
        .dispatch(CHAT, HarnessId::ClaudeCode, request, None)
        .await
        .unwrap();
    wait_for(|| {
        skill(&core).is_some()
            && (!parked
                || core
                    .sessions
                    .session_status(CHAT)
                    .is_some_and(|s| s.status == SessionStatus::Idle))
    })
    .await;
    assert!(
        matches!(skill(&core), Some(MessagePart::Tool { call, subagent_ref: None, .. }) if !call.is_subagent_spawn())
    );
    std::fs::write(dir.path().join("start-child"), "").unwrap();
    wait_for(|| matches!(skill(&core), Some(MessagePart::Tool { call, subagent_ref: Some(doc), subagent_status: Some(SubagentStatus::Running), .. }) if call.is_subagent_spawn() && doc == CHILD)).await;
    assert_eq!(
        core.sessions
            .session_status(CHAT)
            .unwrap()
            .running_subagents,
        1
    );
    if parked {
        assert_eq!(
            core.sessions.session_status(CHAT).unwrap().status,
            SessionStatus::Idle
        );
    }
    std::fs::write(dir.path().join("finish-child"), "").unwrap();
    let expected = if failed {
        SubagentStatus::Failed
    } else {
        SubagentStatus::Done
    };
    wait_for(|| matches!(skill(&core), Some(MessagePart::Tool { subagent_status: Some(status), .. }) if status == expected)
        && core.sessions.session_status(CHAT).is_some_and(|s| s.running_subagents == 0)).await;
    let parent = entries(&core, CHAT);
    assert_eq!(
        parent
            .iter()
            .flat_map(|entry| &entry.parts)
            .filter(|part| matches!(part, MessagePart::Tool { id, .. } if id == "skill"))
            .count(),
        1,
        "refresh must not duplicate the chip in a new turn"
    );
    assert!(
        matches!(skill(&core), Some(MessagePart::Tool { call: ToolCall::Unknown { name, input: None }, .. }) if name == "Agent: code-review"),
        "skill arguments stay out of the synced document"
    );
    let child = entries(&core, CHILD);
    assert!(child.iter().filter(|entry| entry.role == MessageRole::Assistant).flat_map(|entry| &entry.parts).any(|part| matches!(part, MessagePart::Text { text, .. } if text.contains("Reviewing the implementation"))));
    assert!(
        child
            .iter()
            .flat_map(|entry| &entry.parts)
            .any(|part| matches!(part, MessagePart::Tool { id, .. } if id == "child-read"))
    );
    core.sessions.shutdown().await;
    core.doc_host.flush_all();
    let expected_skill = skill(&core);
    drop(core);
    let reopened = assemble(dir.path());
    assert_eq!(skill(&reopened), expected_skill);
    assert_eq!(entries(&reopened, CHILD), child);
}

#[tokio::test]
async fn forked_skill_streams_as_a_visible_linked_subagent() {
    check_forked_skill(false, false).await;
}

#[tokio::test]
async fn forked_skill_updates_its_original_chip_after_parent_completion() {
    check_forked_skill(true, false).await;
}

#[tokio::test]
async fn forked_skill_failure_settles_its_chip_and_count() {
    check_forked_skill(false, true).await;
}
