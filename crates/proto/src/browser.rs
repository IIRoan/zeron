//! Device-local browser automation. Tabs belong to a chat and need explicit consent.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserTab {
    pub id: String,
    pub chat_id: String,
    pub title: String,
    pub url: Option<String>,
    pub loading: bool,
    pub enabled: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum BrowserAction {
    NewTab { url: String },
    Close,
    Navigate { url: String },
    Back,
    Forward,
    Reload,
    Snapshot,
    Screenshot,
    Click { r#ref: String },
    Fill { r#ref: String, value: String },
    Type { text: String },
    Keypress { key: String },
    Scroll { x: f64, y: f64 },
    Select { r#ref: String, value: String },
    Evaluate { script: String },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserRequest {
    pub chat_id: String,
    pub browser_id: String,
    #[serde(flatten)]
    pub command: BrowserAction,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserHostRequest {
    pub request_id: String,
    pub request: BrowserRequest,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserHostReply {
    pub host_id: String,
    pub request_id: String,
    pub result: Result<serde_json::Value, String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn browser_request_roundtrips_and_rejects_unknown_actions() {
        let value = serde_json::json!({"chatId":"chat","browserId":"host/1","action":"fill","ref":"document:1","value":"hello"});
        let request: BrowserRequest = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(request).unwrap(), value);
        assert!(
            serde_json::from_value::<BrowserRequest>(
                serde_json::json!({"chatId":"chat","browserId":"host/1","action":"shell"})
            )
            .is_err()
        );
    }
}
