use std::fs;
use std::net::SocketAddr;

use anyhow::{Context, Result, anyhow, bail};
use oauth2::basic::BasicClient;
use oauth2::{
    AuthUrl, AuthorizationCode, ClientId, ClientSecret, CsrfToken, PkceCodeChallenge, RedirectUrl,
    RefreshToken, Scope, TokenResponse, TokenUrl,
};
use reqwest::Client as HttpClient;
use serde::Deserialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use url::Url;

use crate::config::AppConfig;
use crate::gmail::models::{MessageDetail, MessageSummary, StoredToken};

const AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const GMAIL_API_BASE: &str = "https://gmail.googleapis.com/gmail/v1";
const GMAIL_SCOPES: &[&str] = &[
    "https://www.googleapis.com/auth/gmail.readonly",
    "https://www.googleapis.com/auth/gmail.send",
];

pub trait GmailClient {
    async fn authenticate(&self) -> Result<()>;
    async fn list_inbox(&self, limit: usize) -> Result<Vec<MessageSummary>>;
    async fn read_message(&self, id: &str) -> Result<MessageDetail>;
    async fn send_message(&self, to: &[String], subject: &str, body: &str) -> Result<()>;
}

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

    async fn authorized_http_client(&self) -> Result<(HttpClient, String)> {
        let token = require_token(self)?;
        let access_token = self.ensure_access_token(token).await?;
        let client = HttpClient::builder()
            .build()
            .context("failed to build HTTP client")?;

        Ok((client, access_token))
    }

    async fn ensure_access_token(&self, token: StoredToken) -> Result<String> {
        if let Some(refresh_token) = token.refresh_token.clone() {
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

            let access_token = updated.access_token.clone();
            self.save_token(&updated)?;
            return Ok(access_token);
        }

        Ok(token.access_token)
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
        let (client, access_token) = self.authorized_http_client().await?;
        let max_results = capped.to_string();

        let list_response = client
            .get(format!("{GMAIL_API_BASE}/users/me/messages"))
            .query(&[("labelIds", "INBOX"), ("maxResults", max_results.as_str())])
            .bearer_auth(&access_token)
            .send()
            .await
            .context("failed to query Gmail inbox")?
            .error_for_status()
            .context("Gmail inbox query failed")?
            .json::<ListMessagesResponse>()
            .await
            .context("failed to decode Gmail inbox response")?;

        let mut messages = Vec::new();
        for item in list_response.messages.unwrap_or_default() {
            let detail = client
                .get(format!("{GMAIL_API_BASE}/users/me/messages/{}", item.id))
                .query(&[
                    ("format", "metadata"),
                    ("metadataHeaders", "From"),
                    ("metadataHeaders", "Subject"),
                    ("metadataHeaders", "Date"),
                ])
                .bearer_auth(&access_token)
                .send()
                .await
                .with_context(|| format!("failed to fetch Gmail message {}", item.id))?
                .error_for_status()
                .with_context(|| format!("Gmail returned an error for message {}", item.id))?
                .json::<GmailMessageResponse>()
                .await
                .with_context(|| format!("failed to decode Gmail message {}", item.id))?;

            messages.push(MessageSummary {
                id: item.id,
                from: header_value(&detail.payload, "From")
                    .unwrap_or_else(|| "(unknown sender)".to_string()),
                subject: header_value(&detail.payload, "Subject")
                    .unwrap_or_else(|| "(no subject)".to_string()),
                received_at: header_value(&detail.payload, "Date")
                    .unwrap_or_else(|| "(unknown date)".to_string()),
            });
        }

        Ok(messages)
    }

    async fn read_message(&self, id: &str) -> Result<MessageDetail> {
        self.ensure_configured()?;
        require_token(self)?;

        Ok(MessageDetail {
            id: id.to_string(),
            from: "demo.sender@gmail.com".to_string(),
            to: vec![self.config.gmail.account_email.clone()],
            subject: format!("Opened {}", id),
            body: "This is stub message content. Replace the stub client with Gmail API calls."
                .to_string(),
            received_at: "2026-03-22T09:00:00Z".to_string(),
        })
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

fn require_token(client: &StubGmailClient) -> Result<StoredToken> {
    client.load_token()?.ok_or_else(|| {
        anyhow!("no OAuth token found; run `mailman auth` before using Gmail commands")
    })
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
}

#[derive(Debug, Deserialize)]
struct GmailMessageListItem {
    id: String,
}

#[derive(Debug, Deserialize)]
struct GmailMessageResponse {
    payload: Option<GmailMessagePayload>,
}

#[derive(Debug, Deserialize)]
struct GmailMessagePayload {
    headers: Option<Vec<GmailHeader>>,
}

#[derive(Debug, Deserialize)]
struct GmailHeader {
    name: String,
    value: String,
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
