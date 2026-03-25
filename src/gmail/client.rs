use std::fs;
use std::hash::{Hash, Hasher};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine;
use oauth2::basic::BasicClient;
use oauth2::{
    AuthUrl, AuthorizationCode, ClientId, ClientSecret, CsrfToken, PkceCodeChallenge, RedirectUrl,
    RefreshToken, Scope, TokenResponse, TokenUrl,
};
use reqwest::Client as HttpClient;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use url::Url;

use crate::config::AppConfig;
use crate::gmail::models::{InboxPage, MessageDetail, MessageSummary, StoredToken};
use crate::ui::inbox::FilterMode;

const AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const GMAIL_API_BASE: &str = "https://gmail.googleapis.com/gmail/v1";
const CACHE_TTL_SECS: u64 = 2 * 24 * 60 * 60;
const CACHE_SCHEMA_VERSION: &str = "v4";
const GMAIL_SCOPES: &[&str] = &[
    "https://www.googleapis.com/auth/gmail.readonly",
    "https://www.googleapis.com/auth/gmail.modify",
    "https://www.googleapis.com/auth/gmail.send",
];

pub trait GmailClient {
    async fn authenticate(&self) -> Result<()>;
    async fn list_inbox(&self, limit: usize) -> Result<Vec<MessageSummary>>;
    async fn read_message(&self, id: &str) -> Result<MessageDetail>;
    async fn send_message(&self, to: &[String], subject: &str, body: &str) -> Result<()>;
}

#[derive(Clone)]
pub struct StubGmailClient {
    config: AppConfig,
}

impl StubGmailClient {
    pub fn from_config(config: &AppConfig) -> Self {
        Self {
            config: config.clone(),
        }
    }

    fn ensure_configured(&self) -> Result<()> {
        let gmail = &self.config.gmail;

        if gmail.client_id == "replace-me" || gmail.client_secret == "replace-me" {
            bail!("gmail credentials are not configured; update the generated config file first");
        }

        Ok(())
    }

    pub async fn fetch_inbox_page(
        &self,
        limit: usize,
        page_token: Option<&str>,
        filter: FilterMode,
    ) -> Result<InboxPage> {
        self.ensure_configured()?;
        if let Some(cached) = self.load_inbox_page_cache(limit, page_token, filter)? {
            return Ok(cached);
        }

        let capped = limit.min(50);
        let client = self.authorized_http_client().await?;
        let token = require_token(self)?;

        match self
            .fetch_inbox_page_with_token(&client, &token.access_token, capped, page_token, filter)
            .await
        {
            Ok(page) => {
                self.save_inbox_page_cache(limit, page_token, filter, &page)?;
                Ok(page)
            }
            Err(error) if is_unauthorized(&error) => {
                let refreshed = self.refresh_access_token(token).await?;
                let page = self
                    .fetch_inbox_page_with_token(
                        &client,
                        &refreshed.access_token,
                        capped,
                        page_token,
                        filter,
                    )
                    .await?;
                self.save_inbox_page_cache(limit, page_token, filter, &page)?;
                Ok(page)
            }
            Err(error) => Err(error),
        }
    }

    pub async fn fetch_message_summary(&self, id: &str) -> Result<MessageSummary> {
        self.ensure_configured()?;
        if let Some(cached) = self.load_message_summary_cache(id)? {
            return Ok(cached);
        }

        let client = self.authorized_http_client().await?;
        let token = require_token(self)?;

        match self
            .fetch_message_summary_with_token(&client, &token.access_token, id)
            .await
        {
            Ok(message) => {
                self.save_message_summary_cache(id, &message)?;
                Ok(message)
            }
            Err(error) if is_unauthorized(&error) => {
                let refreshed = self.refresh_access_token(token).await?;
                let message = self
                    .fetch_message_summary_with_token(&client, &refreshed.access_token, id)
                    .await?;
                self.save_message_summary_cache(id, &message)?;
                Ok(message)
            }
            Err(error) => Err(error),
        }
    }

    pub async fn list_user_labels(&self) -> Result<Vec<GmailLabelInfo>> {
        self.ensure_configured()?;
        let client = self.authorized_http_client().await?;
        let token = require_token(self)?;

        match self
            .fetch_labels_with_token(&client, &token.access_token)
            .await
        {
            Ok(labels) => Ok(labels),
            Err(error) if is_unauthorized(&error) => {
                let refreshed = self.refresh_access_token(token).await?;
                self.fetch_labels_with_token(&client, &refreshed.access_token)
                    .await
            }
            Err(error) => Err(error),
        }
    }

    pub async fn apply_or_create_label(
        &self,
        message_id: &str,
        label_name: &str,
    ) -> Result<MessageSummary> {
        self.ensure_configured()?;
        let client = self.authorized_http_client().await?;
        let token = require_token(self)?;

        match self
            .apply_or_create_label_with_token(&client, &token.access_token, message_id, label_name)
            .await
        {
            Ok(summary) => Ok(summary),
            Err(error) if is_unauthorized(&error) => {
                let refreshed = self.refresh_access_token(token).await?;
                self.apply_or_create_label_with_token(
                    &client,
                    &refreshed.access_token,
                    message_id,
                    label_name,
                )
                .await
            }
            Err(error) => Err(error),
        }
    }

    pub async fn remove_label(&self, message_id: &str, label_name: &str) -> Result<MessageSummary> {
        self.ensure_configured()?;
        let client = self.authorized_http_client().await?;
        let token = require_token(self)?;

        match self
            .remove_label_with_token(&client, &token.access_token, message_id, label_name)
            .await
        {
            Ok(summary) => Ok(summary),
            Err(error) if is_unauthorized(&error) => {
                let refreshed = self.refresh_access_token(token).await?;
                self.remove_label_with_token(
                    &client,
                    &refreshed.access_token,
                    message_id,
                    label_name,
                )
                .await
            }
            Err(error) => Err(error),
        }
    }

    fn oauth_client(
        &self,
    ) -> Result<
        BasicClient<
            oauth2::EndpointSet,
            oauth2::EndpointNotSet,
            oauth2::EndpointNotSet,
            oauth2::EndpointNotSet,
            oauth2::EndpointSet,
        >,
    > {
        let auth_url = AuthUrl::new(AUTH_URL.to_string()).context("invalid auth URL")?;
        let token_url = TokenUrl::new(TOKEN_URL.to_string()).context("invalid token URL")?;
        let redirect_url = RedirectUrl::new(self.config.gmail.redirect_url.clone())
            .context("invalid redirect URL in config")?;

        Ok(
            BasicClient::new(ClientId::new(self.config.gmail.client_id.clone()))
                .set_client_secret(ClientSecret::new(self.config.gmail.client_secret.clone()))
                .set_auth_uri(auth_url)
                .set_token_uri(token_url)
                .set_redirect_uri(redirect_url),
        )
    }

    fn save_token(&self, token: &StoredToken) -> Result<()> {
        let token_path = self.config.token_store_path()?;
        let parent = token_path
            .parent()
            .ok_or_else(|| anyhow!("token path has no parent: {}", token_path.display()))?;

        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create data directory {}", parent.display()))?;

        let contents =
            serde_json::to_string_pretty(token).context("failed to serialize token response")?;
        fs::write(&token_path, contents)
            .with_context(|| format!("failed to write token file at {}", token_path.display()))?;

        Ok(())
    }

    fn load_token(&self) -> Result<Option<StoredToken>> {
        let token_path = self.config.token_store_path()?;
        if !token_path.exists() {
            return Ok(None);
        }

        let contents = fs::read_to_string(&token_path)
            .with_context(|| format!("failed to read token file at {}", token_path.display()))?;
        let token = serde_json::from_str(&contents)
            .with_context(|| format!("failed to parse token file at {}", token_path.display()))?;

        Ok(Some(token))
    }

    async fn authorized_http_client(&self) -> Result<HttpClient> {
        let client = HttpClient::builder()
            .build()
            .context("failed to build HTTP client")?;

        Ok(client)
    }

    async fn refresh_access_token(&self, token: StoredToken) -> Result<StoredToken> {
        let refresh_token = token.refresh_token.clone().ok_or_else(|| {
            anyhow!("saved OAuth token has no refresh token; run `mailman auth` again")
        })?;

        let oauth_client = self.oauth_client()?;
        let http_client = HttpClient::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .context("failed to build OAuth HTTP client")?;

        let refreshed = oauth_client
            .exchange_refresh_token(&RefreshToken::new(refresh_token))
            .request_async(&http_client)
            .await
            .context("failed to refresh OAuth access token")?;

        let updated = StoredToken {
            access_token: refreshed.access_token().secret().to_string(),
            refresh_token: refreshed
                .refresh_token()
                .map(RefreshToken::secret)
                .map(ToString::to_string)
                .or(token.refresh_token),
            expires_in_seconds: refreshed.expires_in().map(|duration| duration.as_secs()),
            scopes: refreshed
                .scopes()
                .map(|items| items.iter().map(|scope| scope.to_string()).collect())
                .unwrap_or(token.scopes),
            token_type: Some(format!("{:?}", refreshed.token_type())),
        };

        self.save_token(&updated)?;
        Ok(updated)
    }

    fn cache_dir(&self) -> Result<PathBuf> {
        let dir = AppConfig::cache_dir()?;
        fs::create_dir_all(&dir)
            .with_context(|| format!("failed to create cache directory {}", dir.display()))?;
        Ok(dir)
    }

    fn inbox_page_cache_path(
        &self,
        limit: usize,
        page_token: Option<&str>,
        filter: FilterMode,
    ) -> Result<PathBuf> {
        let suffix = page_token
            .map(stable_hash)
            .unwrap_or_else(|| "first".to_string());
        Ok(self.cache_dir()?.join(format!(
            "{CACHE_SCHEMA_VERSION}_inbox_page_{}_limit_{limit}_{suffix}.json",
            filter.label()
        )))
    }

    fn message_summary_cache_path(&self, id: &str) -> Result<PathBuf> {
        Ok(self.cache_dir()?.join(format!(
            "{CACHE_SCHEMA_VERSION}_message_summary_{}.json",
            sanitize_filename(id)
        )))
    }

    fn message_detail_cache_path(&self, id: &str) -> Result<PathBuf> {
        Ok(self.cache_dir()?.join(format!(
            "{CACHE_SCHEMA_VERSION}_message_detail_{}.json",
            sanitize_filename(id)
        )))
    }

    fn load_inbox_page_cache(
        &self,
        limit: usize,
        page_token: Option<&str>,
        filter: FilterMode,
    ) -> Result<Option<InboxPage>> {
        self.load_cached_value(&self.inbox_page_cache_path(limit, page_token, filter)?)
    }

    fn save_inbox_page_cache(
        &self,
        limit: usize,
        page_token: Option<&str>,
        filter: FilterMode,
        page: &InboxPage,
    ) -> Result<()> {
        self.save_cached_value(
            &self.inbox_page_cache_path(limit, page_token, filter)?,
            page,
        )
    }

    fn load_message_summary_cache(&self, id: &str) -> Result<Option<MessageSummary>> {
        self.load_cached_value(&self.message_summary_cache_path(id)?)
    }

    fn save_message_summary_cache(&self, id: &str, value: &MessageSummary) -> Result<()> {
        self.save_cached_value(&self.message_summary_cache_path(id)?, value)
    }

    fn load_message_detail_cache(&self, id: &str) -> Result<Option<MessageDetail>> {
        self.load_cached_value(&self.message_detail_cache_path(id)?)
    }

    fn save_message_detail_cache(&self, id: &str, value: &MessageDetail) -> Result<()> {
        self.save_cached_value(&self.message_detail_cache_path(id)?, value)
    }

    fn load_cached_value<T: DeserializeOwned>(&self, path: &Path) -> Result<Option<T>> {
        if !path.exists() {
            return Ok(None);
        }

        let raw = fs::read_to_string(path)
            .with_context(|| format!("failed to read cache file {}", path.display()))?;
        let envelope: CacheEnvelope<T> = serde_json::from_str(&raw)
            .with_context(|| format!("failed to parse cache file {}", path.display()))?;

        if cache_is_fresh(envelope.cached_at_epoch_secs) {
            Ok(Some(envelope.value))
        } else {
            Ok(None)
        }
    }

    fn save_cached_value<T: Serialize>(&self, path: &Path, value: &T) -> Result<()> {
        let envelope = CacheEnvelope {
            cached_at_epoch_secs: now_epoch_secs()?,
            value,
        };
        let raw = serde_json::to_string(&envelope).context("failed to serialize cache entry")?;
        fs::write(path, raw)
            .with_context(|| format!("failed to write cache file {}", path.display()))?;
        Ok(())
    }
}

impl GmailClient for StubGmailClient {
    async fn authenticate(&self) -> Result<()> {
        self.ensure_configured()?;
        let token_path = self.config.token_store_path()?;

        let oauth_client = self.oauth_client()?;
        let (pkce_challenge, pkce_verifier) = PkceCodeChallenge::new_random_sha256();

        let mut auth_request = oauth_client
            .authorize_url(CsrfToken::new_random)
            .set_pkce_challenge(pkce_challenge);

        for scope in GMAIL_SCOPES {
            auth_request = auth_request.add_scope(Scope::new((*scope).to_string()));
        }

        let (authorize_url, csrf_state) = auth_request.url();
        let redirect_url = Url::parse(&self.config.gmail.redirect_url)
            .context("failed to parse redirect URL from config")?;
        let callback_addr = redirect_socket_addr(&redirect_url)?;

        println!("Opening browser for Gmail sign-in...");
        println!(
            "If the browser does not open, visit this URL manually:\n{}",
            authorize_url
        );

        webbrowser::open(authorize_url.as_str()).context("failed to open system browser")?;

        let callback = wait_for_callback(callback_addr).await?;
        if callback.state != *csrf_state.secret() {
            bail!("oauth state mismatch; refusing callback");
        }

        let http_client = HttpClient::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .context("failed to build HTTP client")?;

        let token_response = oauth_client
            .exchange_code(AuthorizationCode::new(callback.code))
            .set_pkce_verifier(pkce_verifier)
            .request_async(&http_client)
            .await
            .context("failed to exchange authorization code for tokens")?;

        let scopes = token_response
            .scopes()
            .map(|items| items.iter().map(|scope| scope.to_string()).collect())
            .unwrap_or_else(|| {
                GMAIL_SCOPES
                    .iter()
                    .map(|scope| (*scope).to_string())
                    .collect()
            });

        let token = StoredToken {
            access_token: token_response.access_token().secret().to_string(),
            refresh_token: token_response
                .refresh_token()
                .map(RefreshToken::secret)
                .map(ToString::to_string),
            expires_in_seconds: token_response
                .expires_in()
                .map(|duration| duration.as_secs()),
            scopes,
            token_type: Some(format!("{:?}", token_response.token_type())),
        };

        self.save_token(&token)?;

        println!("Authentication complete.");
        println!("Saved OAuth tokens to {}", token_path.display());

        Ok(())
    }

    async fn list_inbox(&self, limit: usize) -> Result<Vec<MessageSummary>> {
        self.ensure_configured()?;
        let capped = limit.min(50);
        let client = self.authorized_http_client().await?;
        let token = require_token(self)?;

        match self
            .fetch_inbox_messages(&client, &token.access_token, capped)
            .await
        {
            Ok(messages) => Ok(messages),
            Err(error) if is_unauthorized(&error) => {
                let refreshed = self.refresh_access_token(token).await?;
                self.fetch_inbox_messages(&client, &refreshed.access_token, capped)
                    .await
            }
            Err(error) => Err(error),
        }
    }

    async fn read_message(&self, id: &str) -> Result<MessageDetail> {
        self.ensure_configured()?;
        if let Some(cached) = self.load_message_detail_cache(id)? {
            return Ok(cached);
        }

        let client = self.authorized_http_client().await?;
        let token = require_token(self)?;

        match self
            .fetch_message_detail_with_token(&client, &token.access_token, id)
            .await
        {
            Ok(message) => {
                self.save_message_detail_cache(id, &message)?;
                Ok(message)
            }
            Err(error) if is_unauthorized(&error) => {
                let refreshed = self.refresh_access_token(token).await?;
                let message = self
                    .fetch_message_detail_with_token(&client, &refreshed.access_token, id)
                    .await?;
                self.save_message_detail_cache(id, &message)?;
                Ok(message)
            }
            Err(error) => Err(error),
        }
    }

    async fn send_message(&self, to: &[String], subject: &str, body: &str) -> Result<()> {
        self.ensure_configured()?;
        require_token(self)?;

        if to.is_empty() {
            bail!("at least one recipient is required");
        }
        if subject.trim().is_empty() {
            bail!("subject cannot be empty");
        }
        if body.trim().is_empty() {
            bail!("body cannot be empty");
        }

        Ok(())
    }
}

impl StubGmailClient {
    async fn fetch_inbox_messages(
        &self,
        client: &HttpClient,
        access_token: &str,
        limit: usize,
    ) -> Result<Vec<MessageSummary>> {
        let message_ids = self
            .fetch_inbox_page_with_token(client, access_token, limit, None, FilterMode::All)
            .await?;
        let mut messages = Vec::new();
        for id in message_ids.ids {
            messages.push(
                self.fetch_message_summary_with_token(client, access_token, &id)
                    .await?,
            );
        }

        Ok(messages)
    }

    async fn fetch_inbox_page_with_token(
        &self,
        client: &HttpClient,
        access_token: &str,
        limit: usize,
        page_token: Option<&str>,
        filter: FilterMode,
    ) -> Result<InboxPage> {
        let max_results = limit.to_string();
        let mut request = client
            .get(format!("{GMAIL_API_BASE}/users/me/messages"))
            .query(&[("maxResults", max_results.as_str())]);
        for label in gmail_labels_for_filter(filter) {
            request = request.query(&[("labelIds", label)]);
        }
        if let Some(token) = page_token {
            request = request.query(&[("pageToken", token)]);
        }

        let list_response = request
            .bearer_auth(access_token)
            .send()
            .await
            .context("failed to query Gmail inbox")?
            .error_for_status()
            .context("Gmail inbox query failed")?
            .json::<ListMessagesResponse>()
            .await
            .context("failed to decode Gmail inbox response")?;

        Ok(InboxPage {
            ids: list_response
                .messages
                .unwrap_or_default()
                .into_iter()
                .map(|item| item.id)
                .collect(),
            next_page_token: list_response.next_page_token,
        })
    }

    async fn fetch_message_summary_with_token(
        &self,
        client: &HttpClient,
        access_token: &str,
        id: &str,
    ) -> Result<MessageSummary> {
        let detail = client
            .get(format!("{GMAIL_API_BASE}/users/me/messages/{id}"))
            .query(&[
                ("format", "metadata"),
                ("metadataHeaders", "From"),
                ("metadataHeaders", "Subject"),
                ("metadataHeaders", "Date"),
            ])
            .bearer_auth(access_token)
            .send()
            .await
            .with_context(|| format!("failed to fetch Gmail message {id}"))?
            .error_for_status()
            .with_context(|| format!("Gmail returned an error for message {id}"))?
            .json::<GmailMessageResponse>()
            .await
            .with_context(|| format!("failed to decode Gmail message {id}"))?;

        Ok(MessageSummary {
            id: id.to_string(),
            from: header_value(&detail.payload, "From")
                .unwrap_or_else(|| "(unknown sender)".to_string()),
            subject: header_value(&detail.payload, "Subject")
                .unwrap_or_else(|| "(no subject)".to_string()),
            received_at: header_value(&detail.payload, "Date")
                .unwrap_or_else(|| "(unknown date)".to_string()),
            category: classify_message(&detail.label_ids, detail.snippet.as_deref()),
            labels: detail.label_ids.unwrap_or_default(),
            snippet: detail.snippet.unwrap_or_default(),
            provider: "gmail".to_string(),
            account: self.config.gmail.account_email.clone(),
        })
    }

    async fn fetch_message_detail_with_token(
        &self,
        client: &HttpClient,
        access_token: &str,
        id: &str,
    ) -> Result<MessageDetail> {
        let message = client
            .get(format!("{GMAIL_API_BASE}/users/me/messages/{id}"))
            .query(&[("format", "full")])
            .bearer_auth(access_token)
            .send()
            .await
            .with_context(|| format!("failed to fetch Gmail message body for {id}"))?
            .error_for_status()
            .with_context(|| format!("Gmail returned an error for message {id}"))?
            .json::<GmailMessageResponse>()
            .await
            .with_context(|| format!("failed to decode Gmail message body for {id}"))?;

        let payload = message.payload;
        let body = payload
            .as_ref()
            .and_then(extract_best_body)
            .map(format_message_body)
            .unwrap_or_else(|| "(no message body found)".to_string());

        Ok(MessageDetail {
            id: id.to_string(),
            from: header_value(&payload, "From").unwrap_or_else(|| "(unknown sender)".to_string()),
            to: header_list(&payload, "To"),
            subject: header_value(&payload, "Subject")
                .unwrap_or_else(|| "(no subject)".to_string()),
            received_at: header_value(&payload, "Date")
                .unwrap_or_else(|| "(unknown date)".to_string()),
            body,
        })
    }

    async fn fetch_labels_with_token(
        &self,
        client: &HttpClient,
        access_token: &str,
    ) -> Result<Vec<GmailLabelInfo>> {
        let response = client
            .get(format!("{GMAIL_API_BASE}/users/me/labels"))
            .bearer_auth(access_token)
            .send()
            .await
            .context("failed to fetch Gmail labels")?
            .error_for_status()
            .context("Gmail labels query failed")?
            .json::<LabelsListResponse>()
            .await
            .context("failed to decode Gmail labels response")?;

        Ok(response
            .labels
            .unwrap_or_default()
            .into_iter()
            .filter(|label| label.label_type.as_deref() == Some("user"))
            .collect())
    }

    async fn apply_or_create_label_with_token(
        &self,
        client: &HttpClient,
        access_token: &str,
        message_id: &str,
        label_name: &str,
    ) -> Result<MessageSummary> {
        let labels = self.fetch_labels_with_token(client, access_token).await?;
        let label = if let Some(found) = labels.into_iter().find(|label| label.name == label_name) {
            found
        } else {
            client
                .post(format!("{GMAIL_API_BASE}/users/me/labels"))
                .bearer_auth(access_token)
                .json(&CreateLabelRequest {
                    name: label_name.to_string(),
                    label_list_visibility: "labelShow".to_string(),
                    message_list_visibility: "show".to_string(),
                })
                .send()
                .await
                .context("failed to create Gmail label")?
                .error_for_status()
                .context("Gmail label creation failed")?
                .json::<GmailLabelInfo>()
                .await
                .context("failed to decode Gmail label creation response")?
        };

        client
            .post(format!(
                "{GMAIL_API_BASE}/users/me/messages/{message_id}/modify"
            ))
            .bearer_auth(access_token)
            .json(&ModifyLabelsRequest {
                add_label_ids: vec![label.id.clone()],
                remove_label_ids: Vec::new(),
            })
            .send()
            .await
            .with_context(|| format!("failed to apply label '{label_name}'"))?
            .error_for_status()
            .with_context(|| format!("Gmail failed to apply label '{label_name}'"))?;

        let summary = self
            .fetch_message_summary_with_token(client, access_token, message_id)
            .await?;
        self.save_message_summary_cache(message_id, &summary)?;
        Ok(summary)
    }

    async fn remove_label_with_token(
        &self,
        client: &HttpClient,
        access_token: &str,
        message_id: &str,
        label_name: &str,
    ) -> Result<MessageSummary> {
        let labels = self.fetch_labels_with_token(client, access_token).await?;
        let label = labels
            .into_iter()
            .find(|label| label.name == label_name)
            .ok_or_else(|| anyhow!("label '{label_name}' does not exist"))?;

        client
            .post(format!(
                "{GMAIL_API_BASE}/users/me/messages/{message_id}/modify"
            ))
            .bearer_auth(access_token)
            .json(&ModifyLabelsRequest {
                add_label_ids: Vec::new(),
                remove_label_ids: vec![label.id.clone()],
            })
            .send()
            .await
            .with_context(|| format!("failed to remove label '{label_name}'"))?
            .error_for_status()
            .with_context(|| format!("Gmail failed to remove label '{label_name}'"))?;

        let summary = self
            .fetch_message_summary_with_token(client, access_token, message_id)
            .await?;
        self.save_message_summary_cache(message_id, &summary)?;
        Ok(summary)
    }
}

fn require_token(client: &StubGmailClient) -> Result<StoredToken> {
    client.load_token()?.ok_or_else(|| {
        anyhow!("no OAuth token found; run `mailman auth` before using Gmail commands")
    })
}

fn is_unauthorized(error: &anyhow::Error) -> bool {
    error
        .chain()
        .filter_map(|cause| cause.downcast_ref::<reqwest::Error>())
        .any(|reqwest_error| reqwest_error.status() == Some(reqwest::StatusCode::UNAUTHORIZED))
}

struct OAuthCallback {
    code: String,
    state: String,
}

fn redirect_socket_addr(url: &Url) -> Result<SocketAddr> {
    let host = url
        .host_str()
        .ok_or_else(|| anyhow!("redirect URL must include a host"))?;
    let port = url.port_or_known_default().unwrap_or(80);
    let addr = format!("{host}:{port}");

    addr.parse()
        .with_context(|| format!("failed to parse callback socket address {addr}"))
}

async fn wait_for_callback(addr: SocketAddr) -> Result<OAuthCallback> {
    let listener = TcpListener::bind(addr)
        .await
        .with_context(|| format!("failed to bind local callback listener on {addr}"))?;

    let (mut stream, _) = listener
        .accept()
        .await
        .context("failed while waiting for OAuth callback")?;

    let mut buffer = [0_u8; 4096];
    let bytes_read = stream
        .read(&mut buffer)
        .await
        .context("failed reading OAuth callback request")?;
    if bytes_read == 0 {
        bail!("received an empty OAuth callback request");
    }

    let request = String::from_utf8_lossy(&buffer[..bytes_read]);
    let first_line = request
        .lines()
        .next()
        .ok_or_else(|| anyhow!("malformed callback request"))?;

    let path = first_line
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| anyhow!("callback request path missing"))?;

    let callback_url = Url::parse(&format!("http://localhost{path}"))
        .context("failed to parse callback request URL")?;

    let code = callback_url
        .query_pairs()
        .find(|(key, _)| key == "code")
        .map(|(_, value)| value.into_owned())
        .ok_or_else(|| anyhow!("authorization code missing from callback"))?;

    let state = callback_url
        .query_pairs()
        .find(|(key, _)| key == "state")
        .map(|(_, value)| value.into_owned())
        .ok_or_else(|| anyhow!("oauth state missing from callback"))?;

    let response = concat!(
        "HTTP/1.1 200 OK\r\n",
        "Content-Type: text/plain; charset=utf-8\r\n",
        "Connection: close\r\n\r\n",
        "mailman authentication completed. You can close this tab.\n"
    );

    stream
        .write_all(response.as_bytes())
        .await
        .context("failed writing OAuth callback response")?;

    Ok(OAuthCallback { code, state })
}

#[derive(Debug, Deserialize)]
struct ListMessagesResponse {
    messages: Option<Vec<GmailMessageListItem>>,
    #[serde(rename = "nextPageToken")]
    next_page_token: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GmailMessageListItem {
    id: String,
}

#[derive(Debug, Deserialize)]
struct GmailMessageResponse {
    #[serde(rename = "labelIds")]
    label_ids: Option<Vec<String>>,
    snippet: Option<String>,
    payload: Option<GmailMessagePayload>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct GmailLabelInfo {
    pub id: String,
    pub name: String,
    #[serde(rename = "type")]
    label_type: Option<String>,
}

#[derive(Debug, Deserialize)]
struct LabelsListResponse {
    labels: Option<Vec<GmailLabelInfo>>,
}

#[derive(Debug, Serialize)]
struct CreateLabelRequest {
    name: String,
    #[serde(rename = "labelListVisibility")]
    label_list_visibility: String,
    #[serde(rename = "messageListVisibility")]
    message_list_visibility: String,
}

#[derive(Debug, Serialize)]
struct ModifyLabelsRequest {
    #[serde(rename = "addLabelIds")]
    add_label_ids: Vec<String>,
    #[serde(rename = "removeLabelIds")]
    remove_label_ids: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct GmailMessagePayload {
    #[serde(rename = "mimeType")]
    mime_type: Option<String>,
    headers: Option<Vec<GmailHeader>>,
    body: Option<GmailBody>,
    parts: Option<Vec<GmailMessagePayload>>,
}

#[derive(Debug, Deserialize)]
struct GmailBody {
    data: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GmailHeader {
    name: String,
    value: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct CacheEnvelope<T> {
    cached_at_epoch_secs: u64,
    value: T,
}

fn header_value(payload: &Option<GmailMessagePayload>, key: &str) -> Option<String> {
    payload
        .as_ref()
        .and_then(|payload| payload.headers.as_ref())
        .and_then(|headers| {
            headers
                .iter()
                .find(|header| header.name.eq_ignore_ascii_case(key))
        })
        .map(|header| header.value.clone())
}

fn header_list(payload: &Option<GmailMessagePayload>, key: &str) -> Vec<String> {
    header_value(payload, key)
        .map(|value| {
            value
                .split(',')
                .map(|item| item.trim().to_string())
                .filter(|item| !item.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

enum ExtractedBody {
    Plain(String),
    Html(String),
}

fn extract_best_body(payload: &GmailMessagePayload) -> Option<ExtractedBody> {
    if let Some(text) = extract_plain_body(payload) {
        if looks_like_html(&text) {
            return Some(ExtractedBody::Html(text));
        }
        return Some(ExtractedBody::Plain(text));
    }

    extract_html_body(payload).map(ExtractedBody::Html)
}

fn extract_plain_body(payload: &GmailMessagePayload) -> Option<String> {
    if payload.mime_type.as_deref() == Some("text/plain") {
        if let Some(text) = decode_body_data(payload.body.as_ref()) {
            return Some(text);
        }
    }

    payload
        .parts
        .as_ref()
        .and_then(|parts| parts.iter().find_map(extract_plain_body))
}

fn extract_html_body(payload: &GmailMessagePayload) -> Option<String> {
    if payload.mime_type.as_deref() == Some("text/html") {
        if let Some(text) = decode_body_data(payload.body.as_ref()) {
            return Some(text);
        }
    }

    payload
        .parts
        .as_ref()
        .and_then(|parts| parts.iter().find_map(extract_html_body))
        .or_else(|| decode_body_data(payload.body.as_ref()))
}

fn decode_body_data(body: Option<&GmailBody>) -> Option<String> {
    let data = body?.data.as_ref()?;
    let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(data)
        .or_else(|_| base64::engine::general_purpose::URL_SAFE.decode(data))
        .ok()?;
    String::from_utf8(decoded).ok()
}

fn looks_like_html(text: &str) -> bool {
    let lower = text.trim_start().to_ascii_lowercase();
    lower.starts_with("<!doctype html")
        || lower.starts_with("<html")
        || lower.starts_with("<body")
        || lower.contains("<head")
        || lower.contains("<table")
        || lower.contains("<div")
        || lower.contains("<span")
        || lower.contains("<a href=")
        || lower.contains("</p>")
}

fn format_message_body(body: ExtractedBody) -> String {
    let normalized = match body {
        ExtractedBody::Plain(text) => text.replace("\r\n", "\n").replace('\r', "\n"),
        ExtractedBody::Html(html) => {
            html2text::from_read(remove_html_sections(&html).as_bytes(), 80)
                .unwrap_or(html)
                .replace("\r\n", "\n")
                .replace('\r', "\n")
        }
    };

    let cleaned = shorten_urls(&decode_html_entities(&normalized));
    let wrapped = cleaned
        .lines()
        .map(|line| wrap_text_line(&normalize_whitespace(line), 72))
        .collect::<Vec<_>>()
        .join("\n");

    collapse_blank_lines(&wrapped)
}

fn remove_html_sections(input: &str) -> String {
    let mut output = input.to_string();
    for tag in ["script", "style", "head", "title"] {
        output = remove_tag_block(&output, tag);
    }
    output
}

fn remove_tag_block(input: &str, tag: &str) -> String {
    let mut result = String::new();
    let mut rest = input;
    let open = format!("<{tag}");
    let close = format!("</{tag}>");

    loop {
        let Some(start) = rest.to_ascii_lowercase().find(&open) else {
            result.push_str(rest);
            break;
        };
        result.push_str(&rest[..start]);
        let after_start = &rest[start..];
        let lower_after = after_start.to_ascii_lowercase();
        let Some(end) = lower_after.find(&close) else {
            break;
        };
        let close_end = end + close.len();
        rest = &after_start[close_end..];
    }

    result
}

fn decode_html_entities(input: &str) -> String {
    input
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
}

fn shorten_urls(input: &str) -> String {
    input
        .split_whitespace()
        .map(|token| {
            if (token.starts_with("http://") || token.starts_with("https://")) && token.len() > 60 {
                "[link]".to_string()
            } else {
                token.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn normalize_whitespace(line: &str) -> String {
    line.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn wrap_text_line(line: &str, max_width: usize) -> String {
    if line.is_empty() {
        return String::new();
    }

    let mut out = String::new();
    let mut current_len = 0usize;

    for word in line.split_whitespace() {
        let word_len = word.chars().count();
        if current_len == 0 {
            if word_len <= max_width {
                out.push_str(word);
                current_len = word_len;
            } else {
                out.push_str(&hard_wrap_word(word, max_width));
                current_len = out
                    .lines()
                    .last()
                    .map(|line| line.chars().count())
                    .unwrap_or(0);
            }
            continue;
        }

        if current_len + 1 + word_len <= max_width {
            out.push(' ');
            out.push_str(word);
            current_len += 1 + word_len;
        } else {
            out.push('\n');
            if word_len <= max_width {
                out.push_str(word);
                current_len = word_len;
            } else {
                out.push_str(&hard_wrap_word(word, max_width));
                current_len = out
                    .lines()
                    .last()
                    .map(|line| line.chars().count())
                    .unwrap_or(0);
            }
        }
    }

    out
}

fn hard_wrap_word(word: &str, max_width: usize) -> String {
    let mut out = String::new();
    let mut count = 0usize;
    for ch in word.chars() {
        if count == max_width {
            out.push('\n');
            count = 0;
        }
        out.push(ch);
        count += 1;
    }
    out
}

fn collapse_blank_lines(input: &str) -> String {
    let mut output = String::new();
    let mut blank_count = 0usize;

    for line in input.lines() {
        if line.trim().is_empty() {
            blank_count += 1;
            if blank_count > 1 {
                continue;
            }
        } else {
            blank_count = 0;
        }

        if !output.is_empty() {
            output.push('\n');
        }
        output.push_str(line.trim_end());
    }

    output
}

fn now_epoch_secs() -> Result<u64> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before UNIX epoch")?
        .as_secs())
}

fn cache_is_fresh(cached_at_epoch_secs: u64) -> bool {
    now_epoch_secs()
        .map(|now| now.saturating_sub(cached_at_epoch_secs) <= CACHE_TTL_SECS)
        .unwrap_or(false)
}

fn sanitize_filename(input: &str) -> String {
    input
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

fn stable_hash(input: &str) -> String {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    input.hash(&mut hasher);
    format!("{:x}", hasher.finish())
}

fn classify_message(label_ids: &Option<Vec<String>>, snippet: Option<&str>) -> String {
    if let Some(labels) = label_ids {
        if labels.iter().any(|label| label == "CATEGORY_PROMOTIONS") {
            return "Promotions".to_string();
        }
        if labels.iter().any(|label| label == "CATEGORY_SOCIAL") {
            return "Social".to_string();
        }
        if labels.iter().any(|label| label == "CATEGORY_UPDATES") {
            return "Updates".to_string();
        }
        if labels.iter().any(|label| label == "CATEGORY_FORUMS") {
            return "Forums".to_string();
        }
        if labels.iter().any(|label| label == "CATEGORY_PERSONAL") {
            return "Primary".to_string();
        }
    }

    let lowered = snippet.unwrap_or_default().to_ascii_lowercase();
    if lowered.contains("unsubscribe") || lowered.contains("newsletter") {
        "Promotion".to_string()
    } else {
        "Primary".to_string()
    }
}

fn gmail_labels_for_filter(filter: FilterMode) -> &'static [&'static str] {
    match filter {
        FilterMode::All => &["INBOX"],
        FilterMode::Primary => &["INBOX", "CATEGORY_PERSONAL"],
        FilterMode::Promotions => &["INBOX", "CATEGORY_PROMOTIONS"],
        FilterMode::Updates => &["INBOX", "CATEGORY_UPDATES"],
        FilterMode::Social => &["INBOX", "CATEGORY_SOCIAL"],
        FilterMode::Forums => &["INBOX", "CATEGORY_FORUMS"],
        FilterMode::Important => &["INBOX", "IMPORTANT"],
        FilterMode::Spam => &["SPAM"],
        FilterMode::Unread => &["INBOX", "UNREAD"],
    }
}
