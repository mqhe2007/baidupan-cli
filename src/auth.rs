use std::time::Duration;

use reqwest::Client;
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::Value;
use tokio::time::sleep;

use crate::config::{current_unix_timestamp, AppCredentials, StoredToken, USER_AGENT};
use crate::{Error, Result};

const OAUTH_BASE: &str = "https://openapi.baidu.com/oauth/2.0";
const OAUTH_SCOPE: &str = "basic,netdisk";

#[derive(Debug, Clone, Deserialize)]
pub struct DeviceCodeResponse {
    pub device_code: String,
    pub user_code: String,
    pub verification_url: String,
    pub qrcode_url: String,
    pub expires_in: u64,
    pub interval: u64,
}

#[derive(Debug, Clone, Deserialize)]
struct TokenResponse {
    access_token: String,
    expires_in: i64,
    refresh_token: String,
    scope: Option<String>,
}

#[derive(Debug, Clone)]
pub struct OAuthClient {
    http: Client,
}

impl OAuthClient {
    pub fn new() -> Result<Self> {
        let http = Client::builder().user_agent(USER_AGENT).build()?;
        Ok(Self { http })
    }

    pub async fn request_device_code(
        &self,
        credentials: &AppCredentials,
    ) -> Result<DeviceCodeResponse> {
        let response = self
            .http
            .get(format!("{OAUTH_BASE}/device/code"))
            .query(&[
                ("response_type", "device_code"),
                ("client_id", credentials.app_key.as_str()),
                ("scope", OAUTH_SCOPE),
            ])
            .send()
            .await?;

        parse_oauth_success(response).await
    }

    pub async fn poll_for_token(
        &self,
        credentials: &AppCredentials,
        device_code: &DeviceCodeResponse,
    ) -> Result<StoredToken> {
        let deadline = current_unix_timestamp()? + device_code.expires_in as i64;
        let mut interval = device_code.interval.max(5);

        loop {
            if current_unix_timestamp()? >= deadline {
                return Err(Error::Api(
                    "device code expired before authorization completed".to_string(),
                ));
            }

            let response = self
                .http
                .get(format!("{OAUTH_BASE}/token"))
                .query(&[
                    ("grant_type", "device_token"),
                    ("code", device_code.device_code.as_str()),
                    ("client_id", credentials.app_key.as_str()),
                    ("client_secret", credentials.app_secret.as_str()),
                ])
                .send()
                .await?;

            let payload: Value = response.json().await?;
            if let Some(code) = payload.get("error").and_then(Value::as_str) {
                match code {
                    "authorization_pending" => {
                        sleep(Duration::from_secs(interval)).await;
                        continue;
                    }
                    "slow_down" => {
                        interval += 5;
                        sleep(Duration::from_secs(interval)).await;
                        continue;
                    }
                    "expired_token" | "code_expired" => {
                        return Err(Error::Api(
                            "device code expired; run login again".to_string(),
                        ));
                    }
                    "authorization_declined" | "access_denied" => {
                        return Err(Error::Api(
                            "authorization was denied by the user".to_string(),
                        ));
                    }
                    _ => {
                        return Err(oauth_value_error(&payload));
                    }
                }
            }

            let token: TokenResponse = serde_json::from_value(payload)?;
            return token.into_stored_token();
        }
    }

    pub async fn refresh_token(
        &self,
        credentials: &AppCredentials,
        refresh_token: &str,
    ) -> Result<StoredToken> {
        let response = self
            .http
            .get(format!("{OAUTH_BASE}/token"))
            .query(&[
                ("grant_type", "refresh_token"),
                ("refresh_token", refresh_token),
                ("client_id", credentials.app_key.as_str()),
                ("client_secret", credentials.app_secret.as_str()),
            ])
            .send()
            .await?;

        let token: TokenResponse = parse_oauth_success(response).await?;
        token.into_stored_token()
    }
}

impl TokenResponse {
    fn into_stored_token(self) -> Result<StoredToken> {
        Ok(StoredToken {
            access_token: self.access_token,
            refresh_token: self.refresh_token,
            expires_at: current_unix_timestamp()? + self.expires_in,
            scope: self.scope,
        })
    }
}

async fn parse_oauth_success<T>(response: reqwest::Response) -> Result<T>
where
    T: DeserializeOwned,
{
    let payload: Value = response.json().await?;
    if payload.get("error").is_some() {
        return Err(oauth_value_error(&payload));
    }
    Ok(serde_json::from_value(payload)?)
}

fn oauth_value_error(payload: &Value) -> Error {
    let code = payload
        .get("error")
        .and_then(Value::as_str)
        .unwrap_or("unknown_oauth_error");
    let description = payload
        .get("error_description")
        .and_then(Value::as_str)
        .unwrap_or("no description provided");
    Error::Api(format!("{code}: {description}"))
}
