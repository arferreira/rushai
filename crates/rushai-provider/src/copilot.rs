//! GitHub Copilot: device-code sign-in and the chat provider.
//!
//! Two-layer auth: the device flow yields a long-lived `ghu_` token, which
//! is exchanged for a short-lived `tk_` API token (~30 minutes) that the
//! chat endpoint accepts.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Deserialize;
use serde_json::Value;
use tokio::sync::Mutex;

use crate::openai_compat::{chat_body, chat_event_stream, check_status};
use crate::{ChatRequest, EventStream, ModelInfo, Provider, ProviderError};

const CLIENT_ID: &str = "Iv1.b507a08c87ecfe98";
const EDITOR_VERSION: &str = "vscode/1.99.0";
const PLUGIN_VERSION: &str = "copilot-chat/0.26.0";
const INTEGRATION_ID: &str = "vscode-chat";
const USER_AGENT: &str = "GitHubCopilotChat/0.26.0";
/// Refresh the API token when it expires within this window.
const REFRESH_MARGIN: Duration = Duration::from_secs(120);

pub struct CopilotAuth {
    client: reqwest::Client,
    github_base: String,
    api_base: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DeviceCode {
    pub user_code: String,
    pub verification_uri: String,
    device_code: String,
    interval: u64,
    expires_in: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ApiToken {
    pub(crate) token: String,
    pub(crate) expires_at: u64,
}

impl ApiToken {
    fn needs_refresh(&self) -> bool {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        now + REFRESH_MARGIN >= Duration::from_secs(self.expires_at)
    }
}

impl Default for CopilotAuth {
    fn default() -> Self {
        Self::new()
    }
}

impl CopilotAuth {
    pub fn new() -> Self {
        Self::with_base_urls("https://github.com".into(), "https://api.github.com".into())
    }

    pub fn with_base_urls(github_base: String, api_base: String) -> Self {
        Self {
            client: reqwest::Client::new(),
            github_base,
            api_base,
        }
    }

    pub async fn start(&self) -> Result<DeviceCode, ProviderError> {
        let response = self
            .client
            .post(format!("{}/login/device/code", self.github_base))
            .header("accept", "application/json")
            .form(&[("client_id", CLIENT_ID), ("scope", "read:user")])
            .send()
            .await?;
        let response = check_status(response).await?;
        Ok(response.json().await?)
    }

    /// Poll until the user approves in the browser. Returns the `ghu_` token.
    pub async fn poll(&self, code: &DeviceCode) -> Result<String, ProviderError> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(code.expires_in);
        let mut interval = Duration::from_secs(code.interval.max(1));
        loop {
            tokio::time::sleep(interval).await;
            if tokio::time::Instant::now() >= deadline {
                return Err(ProviderError::Api {
                    status: 0,
                    message: "device code expired before approval".into(),
                    retry_after: None,
                });
            }
            let response = self
                .client
                .post(format!("{}/login/oauth/access_token", self.github_base))
                .header("accept", "application/json")
                .form(&[
                    ("client_id", CLIENT_ID),
                    ("device_code", code.device_code.as_str()),
                    ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ])
                .send()
                .await?;
            let body: Value = check_status(response).await?.json().await?;
            if let Some(token) = body["access_token"].as_str() {
                return Ok(token.to_owned());
            }
            match body["error"].as_str().unwrap_or_default() {
                "authorization_pending" => {}
                "slow_down" => interval += Duration::from_secs(5),
                error => {
                    return Err(ProviderError::Api {
                        status: 0,
                        message: format!("device flow failed: {error}"),
                        retry_after: None,
                    });
                }
            }
        }
    }

    pub(crate) async fn exchange(&self, github_token: &str) -> Result<ApiToken, ProviderError> {
        let response = self
            .client
            .get(format!("{}/copilot_internal/v2/token", self.api_base))
            .header("authorization", format!("token {github_token}"))
            .header("editor-version", EDITOR_VERSION)
            .header("editor-plugin-version", PLUGIN_VERSION)
            .header("user-agent", USER_AGENT)
            .send()
            .await?;
        let response = check_status(response).await?;
        Ok(response.json().await?)
    }
}

pub struct Copilot {
    client: reqwest::Client,
    auth: CopilotAuth,
    github_token: String,
    chat_base: String,
    model: ModelInfo,
    token: Mutex<Option<ApiToken>>,
}

impl Copilot {
    pub fn new(github_token: String, model: ModelInfo) -> Self {
        Self::with_base_urls(
            github_token,
            model,
            CopilotAuth::new(),
            "https://api.githubcopilot.com".into(),
        )
    }

    pub fn with_base_urls(
        github_token: String,
        model: ModelInfo,
        auth: CopilotAuth,
        chat_base: String,
    ) -> Self {
        Self {
            client: reqwest::Client::new(),
            auth,
            github_token,
            chat_base,
            model,
            token: Mutex::new(None),
        }
    }

    async fn fresh_token(&self) -> Result<String, ProviderError> {
        let mut slot = self.token.lock().await;
        if let Some(token) = slot.as_ref()
            && !token.needs_refresh()
        {
            return Ok(token.token.clone());
        }
        let token = self.auth.exchange(&self.github_token).await?;
        let value = token.token.clone();
        *slot = Some(token);
        Ok(value)
    }
}

#[async_trait::async_trait]
impl Provider for Copilot {
    fn model(&self) -> &ModelInfo {
        &self.model
    }

    async fn stream(&self, request: ChatRequest) -> Result<EventStream, ProviderError> {
        let token = self.fresh_token().await?;
        let response = self
            .client
            .post(format!("{}/chat/completions", self.chat_base))
            .bearer_auth(token)
            .header("editor-version", EDITOR_VERSION)
            .header("editor-plugin-version", PLUGIN_VERSION)
            .header("copilot-integration-id", INTEGRATION_ID)
            .header("user-agent", USER_AGENT)
            .json(&chat_body(&self.model, &request))
            .send()
            .await?;
        Ok(chat_event_stream(check_status(response).await?))
    }
}
