#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MessageSummary {
    pub id: String,
    pub from: String,
    pub subject: String,
    pub received_at: String,
    pub category: String,
    pub labels: Vec<String>,
    pub snippet: String,
    pub provider: String,
    pub account: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MessageDetail {
    pub id: String,
    pub from: String,
    pub to: Vec<String>,
    pub subject: String,
    pub body: String,
    pub received_at: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct InboxPage {
    pub ids: Vec<String>,
    pub next_page_token: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct StoredToken {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_in_seconds: Option<u64>,
    pub scopes: Vec<String>,
    pub token_type: Option<String>,
}
