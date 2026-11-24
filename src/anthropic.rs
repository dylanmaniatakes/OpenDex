//! Anthropic account sign-in. This uses the provider's OAuth flow and stores
//! its refresh token in the system credential store.
//!
//! OpenDex only asks Anthropic for the inference and profile scopes. Account
//! sessions therefore use the official endpoint because the token was granted
//! by Anthropic and cannot be pointed at another service.
use crate::state::{
    AnthropicAuth, Provider, ProviderKind, delete_secret, read_secret, write_secret,
};
use anyhow::{Context, Result, bail, ensure};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use reqwest::header;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    sync::OnceLock,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
use tokio::sync::Mutex;

const CLIENT_ID: &str = "22422756-60c9-4084-8eb7-27705fd5cf9a";
const AUTHORIZE_URL: &str = "https://claude.com/cai/oauth/authorize";
const TOKEN_URL: &str = "https://platform.claude.com/v1/oauth/token";
const PROFILE_URL: &str = "https://api.anthropic.com/api/oauth/profile";
/// Inference and profile are enough to use Claude. Account sessions do not
/// create API keys or access other Anthropic features.
const SCOPES: &str = "user:inference user:profile";
const OAUTH_BETA: &str = "oauth-2025-04-20";
const TIMEOUT: Duration = Duration::from_secs(300);

/// Anthropic returns the session credentials and refresh token from OAuth.

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Credentials {
    access_token: String,
    refresh_token: String,
    expires_at_ms: u64,
    #[serde(default)]
    account: Option<String>,
    #[serde(default)]
    organization: Option<String>,
    #[serde(default)]
    subscription: Option<String>,
    #[serde(default)]
    scope: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct Account {
    pub connected: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub organization: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subscription: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_in: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manual_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn random_bytes() -> String {
    let mut bytes = Vec::with_capacity(48);
    while bytes.len() < 48 {
        bytes.extend_from_slice(uuid::Uuid::new_v4().as_bytes());
    }
    URL_SAFE_NO_PAD.encode(bytes)
}

fn code_challenge(verifier: &str) -> String {
    let digest = Sha256::digest(verifier.as_bytes());
    URL_SAFE_NO_PAD.encode(digest)
}

fn account_credentials(provider: &Provider) -> Result<Option<Credentials>> {
    ensure!(
        provider.kind == ProviderKind::Anthropic
            && provider.anthropic_auth == AnthropicAuth::Account,
        "That connection does not use Anthropic account sign-in."
    );
    let encoded = read_secret(
        provider.credential_id(),
        &provider.endpoint,
        provider.has_secret,
    )?;
    let Some(encoded) = encoded else {
        return Ok(None);
    };
    let credentials = serde_json::from_str::<Credentials>(&encoded)
        .map_err(|_| anyhow::anyhow!("This account session is not readable. Sign in again."))?;
    Ok(Some(credentials))
}

pub async fn account_status(provider: &Provider) -> Result<Account> {
    match account_credentials(provider)? {
        None => Ok(Account {
            connected: false,
            manual_url: None,
            email: None,
            organization: None,
            subscription: None,
            expires_in: None,
            error: None,
        }),
        Some(credentials) => {
            let now = now_ms();
            Ok(Account {
                connected: true,
                email: credentials.account.clone(),
                organization: credentials.organization.clone(),
                subscription: credentials.subscription.clone(),
                expires_in: Some(credentials.expires_at_ms.saturating_sub(now) / 1000),
                manual_url: None,
                error: if credentials.expires_at_ms <= now {
                    Some(
                        "The access token has expired; OpenDex refreshes it automatically."
                            .to_string(),
                    )
                } else {
                    None
                },
            })
        }
    }
}

pub async fn request(
    client: &reqwest::Client,
    provider: &Provider,
    request: reqwest::RequestBuilder,
) -> Result<reqwest::RequestBuilder> {
    if provider.anthropic_auth == AnthropicAuth::Api {
        return Ok(request);
    }
    let credentials = match account_credentials(provider)? {
        Some(credentials) if credentials.expires_at_ms > now_ms() + 60_000 => credentials,
        Some(_) => ensure_credentials(client, provider).await?,
        None => bail!("Connect the Anthropic account before using it."),
    };
    let mut request = request.bearer_auth(&credentials.access_token);
    if let Ok(value) = header::HeaderValue::from_str(OAUTH_BETA) {
        request = request.header("anthropic-beta", value);
    }
    Ok(request)
}

async fn ensure_credentials(client: &reqwest::Client, provider: &Provider) -> Result<Credentials> {
    let _lock = refresh_lock().lock().await;
    let now = now_ms();
    let latest =
        account_credentials(provider)?.context("Connect the Anthropic account before using it.")?;
    if latest.expires_at_ms > now + 60_000 {
        return Ok(latest);
    }
    refresh(client, provider, latest).await
}

fn refresh_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(Mutex::default)
}

async fn refresh(
    client: &reqwest::Client,
    provider: &Provider,
    previous: Credentials,
) -> Result<Credentials> {
    let response = client
        .post(TOKEN_URL)
        .json(&serde_json::json!({
            "grant_type": "refresh_token",
            "refresh_token": previous.refresh_token,
            "client_id": CLIENT_ID,
        }))
        .send()
        .await
        .context("Could not reach the Anthropic token endpoint.")?;
    if !response.status().is_success() {
        let text = response.text().await.unwrap_or_default();
        bail!("Could not refresh the Anthropic account session. Sign in again. ({text})");
    }
    let body = response.json::<serde_json::Value>().await?;
    save_token(provider, &body, Some(previous)).await
}

async fn save_token(
    provider: &Provider,
    body: &serde_json::Value,
    previous: Option<Credentials>,
) -> Result<Credentials> {
    let credentials = parse_token(body, previous.as_ref(), now_ms())?;
    let encoded = serde_json::to_string(&credentials)?;
    write_secret(provider.credential_id(), &provider.endpoint, &encoded)?;
    Ok(credentials)
}

fn parse_token(
    body: &serde_json::Value,
    previous: Option<&Credentials>,
    now: u64,
) -> Result<Credentials> {
    let access_token = body
        .get("access_token")
        .and_then(|value| value.as_str())
        .filter(|value| !value.is_empty())
        .context("Anthropic did not return an access token.")?;
    let refresh_token = body
        .get("refresh_token")
        .and_then(|value| value.as_str())
        .map(String::from)
        .or_else(|| previous.map(|item| item.refresh_token.clone()))
        .context("Anthropic did not return a refresh token.")?;
    let expires_in = body
        .get("expires_in")
        .and_then(|value| value.as_u64())
        .unwrap_or(3600);
    let credentials = Credentials {
        access_token: access_token.to_string(),
        refresh_token,
        expires_at_ms: now.saturating_add(expires_in.saturating_mul(1_000)),
        account: body
            .get("account")
            .and_then(|value| value.get("email_address"))
            .and_then(|value| value.as_str())
            .map(String::from),
        organization: body
            .get("organization")
            .and_then(|value| value.get("uuid"))
            .and_then(|value| value.as_str())
            .map(String::from),
        subscription: previous.and_then(|item| item.subscription.clone()),
        scope: body
            .get("scope")
            .and_then(|value| value.as_str())
            .map(String::from),
    };
    Ok(Credentials {
        account: credentials
            .account
            .or_else(|| previous.and_then(|item| item.account.clone())),
        organization: credentials
            .organization
            .or_else(|| previous.and_then(|item| item.organization.clone())),
        scope: credentials
            .scope
            .or_else(|| previous.and_then(|item| item.scope.clone())),
        ..credentials
    })
}

async fn exchange_code(
    client: &reqwest::Client,
    payload: &serde_json::Value,
) -> Result<serde_json::Value> {
    let response = client
        .post(TOKEN_URL)
        .json(payload)
        .send()
        .await
        .context("Could not reach the Anthropic token endpoint.")?;
    if !response.status().is_success() {
        let text = response.text().await.unwrap_or_default();
        bail!("Anthropic sign-in failed. Re-check the account step. ({text})");
    }
    let body = response.json::<serde_json::Value>().await?;
    Ok(body)
}

async fn profile(client: &reqwest::Client, credentials: &Credentials) -> Result<Account> {
    let response = client
        .get(PROFILE_URL)
        .bearer_auth(&credentials.access_token)
        .send()
        .await
        .context("Could not read the Anthropic account profile.")?;
    let body = if response.status().is_success() {
        response
            .json::<serde_json::Value>()
            .await
            .unwrap_or_default()
    } else {
        serde_json::Value::default()
    };
    let account = body
        .get("account")
        .and_then(|value| value.get("email"))
        .and_then(|value| value.as_str())
        .map(String::from);
    let organization = body
        .get("organization")
        .and_then(|value| value.get("uuid"))
        .and_then(|value| value.as_str())
        .map(String::from);
    let subscription = body
        .get("organization")
        .and_then(|value| value.get("organization_type"))
        .and_then(|value| value.as_str())
        .map(String::from);
    Ok(Account {
        connected: true,
        email: account.or_else(|| credentials.account.clone()),
        organization: organization.or_else(|| credentials.organization.clone()),
        subscription,
        expires_in: Some(credentials.expires_at_ms.saturating_sub(now_ms()) / 1000),
        manual_url: None,
        error: None,
    })
}

fn authorize_url(verifier: &str, state: &str, port: u16, manual: bool) -> Result<reqwest::Url> {
    let mut url = reqwest::Url::parse(AUTHORIZE_URL)?;
    let callback = if manual {
        "https://platform.claude.com/oauth/code/callback".to_string()
    } else {
        format!("http://localhost:{port}/callback")
    };
    let mut query = url.query_pairs_mut();
    query.append_pair("code", "true");
    query.append_pair("client_id", CLIENT_ID);
    query.append_pair("response_type", "code");
    query.append_pair("redirect_uri", &callback);
    query.append_pair("scope", SCOPES);
    query.append_pair("code_challenge", &code_challenge(verifier));
    query.append_pair("code_challenge_method", "S256");
    query.append_pair("state", state);
    drop(query);
    Ok(url)
}

async fn read_callback(listener: &mut tokio::net::TcpListener, state: &str) -> Result<String> {
    let (socket, _) = listener
        .accept()
        .await
        .context("Could not receive the Anthropic sign-in callback.")?;
    let (read, mut write) = socket.into_split();
    let mut read = tokio::io::BufReader::new(read);
    let mut request = String::new();
    read.read_line(&mut request).await?;
    let first_line = request.lines().next().unwrap_or_default();
    let path = first_line.split_whitespace().nth(1).unwrap_or_default();
    let url = reqwest::Url::parse(&format!("http://localhost{path}"))
        .map_err(|error| anyhow::anyhow!("Received an invalid Anthropic callback. ({error})"))?;
    let code = url
        .query_pairs()
        .find(|(key, _)| key == "code")
        .map(|(_, value)| value.to_string());
    let returned_state = url
        .query_pairs()
        .find(|(key, _)| key == "state")
        .map(|(_, value)| value.to_string())
        .unwrap_or_default();
    let success = code.is_some() && returned_state == state;
    let response = if success {
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\n\r\n<h1>Signed in to Anthropic</h1><p>Return to OpenDex.</p>"
    } else {
        "HTTP/1.1 400 Bad Request\r\nContent-Type: text/html; charset=utf-8\r\n\r\n<h1>Authentication error</h1><p>Try again from OpenDex.</p>"
    };
    write.write_all(response.as_bytes()).await?;
    write.shutdown().await?;
    if !success {
        bail!("The Anthropic browser sign-in did not return a valid code.");
    }
    code.context("The Anthropic browser sign-in did not return a valid code.")
}

pub fn disconnect(provider: &Provider) -> Result<Provider> {
    ensure!(
        provider.kind == ProviderKind::Anthropic
            && provider.anthropic_auth == AnthropicAuth::Account,
        "That connection does not use Anthropic account sign-in."
    );
    let mut provider = provider.clone();
    delete_secret(provider.credential_id(), &provider.endpoint)?;
    provider.has_secret = false;
    Ok(provider)
}

pub async fn connect(
    client: &reqwest::Client,
    mut provider: Provider,
) -> Result<(Provider, Account)> {
    ensure!(
        provider.kind == ProviderKind::Anthropic,
        "Account sign-in is available for Anthropic connections."
    );
    ensure!(
        provider.anthropic_auth == AnthropicAuth::Account,
        "Save the connection in account mode before signing in."
    );
    provider.validate()?;
    if provider.has_secret {
        bail!("Forget the saved API key before signing in to an Anthropic account.");
    }
    let verifier = random_bytes();
    let state = random_bytes();
    let mut listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .context("Could not open a local Anthropic callback listener.")?;
    let port = listener
        .local_addr()
        .context("Could not determine the Anthropic callback port.")?
        .port();
    let automatic_url = authorize_url(&verifier, &state, port, false)?;
    let _opened = open_browser(automatic_url.as_str());
    let code = tokio::time::timeout(TIMEOUT, read_callback(&mut listener, &state)).await;
    let code = match code {
        Ok(Ok(code)) => code,
        Ok(Err(error)) => return Err(error),
        Err(_) => bail!("Anthropic sign-in timed out before its browser callback."),
    };
    provider.anthropic_auth = AnthropicAuth::Account;
    let credentials = exchange_code(
        client,
        &serde_json::json!({
            "grant_type": "authorization_code",
            "code": code,
            "redirect_uri": format!("http://localhost:{port}/callback"),
            "client_id": CLIENT_ID,
            "code_verifier": verifier,
            "state": state,
        }),
    )
    .await?;
    let credentials = save_token(&provider, &credentials, None).await?;
    let account = profile(client, &credentials).await?;
    provider.has_secret = true;
    let encoded = serde_json::to_string(&credentials)?;
    write_secret(provider.credential_id(), &provider.endpoint, &encoded)?;
    Ok((provider, account))
}

fn open_browser(url: &str) -> bool {
    #[cfg(target_os = "macos")]
    let mut command = std::process::Command::new("/usr/bin/open");
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut command = std::process::Command::new("cmd");
        command.args(["/C", "start", "", url]);
        command
    };
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let mut command = std::process::Command::new("xdg-open");
    #[cfg(not(target_os = "windows"))]
    command.arg(url);
    command.status().is_ok_and(|status| status.success())
}

pub fn ensure_api_fallback(provider: &mut Provider) {
    if provider.anthropic_auth == AnthropicAuth::Account && !provider.has_secret {
        provider.anthropic_auth = AnthropicAuth::Api;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn refresh_preserves_identity_and_accepts_rotated_tokens() {
        let previous = Credentials {
            access_token: "expired".into(),
            refresh_token: "old-refresh".into(),
            expires_at_ms: 1,
            account: Some("person@example.org".into()),
            organization: Some("organization".into()),
            subscription: Some("subscription".into()),
            scope: Some("user:inference".into()),
        };
        let renewed = parse_token(
            &json!({"access_token":"renewed", "expires_in":3600}),
            Some(&previous),
            1000,
        )
        .unwrap();
        assert_eq!(renewed.refresh_token, "old-refresh");
        assert_eq!(renewed.account, previous.account);
        assert_eq!(renewed.organization, previous.organization);
        assert_eq!(renewed.subscription, previous.subscription);
        assert_eq!(renewed.scope, previous.scope);
        assert_eq!(renewed.expires_at_ms, 3_601_000);

        let rotated = parse_token(
            &json!({"access_token":"new", "refresh_token":"rotated", "expires_in":3600}),
            Some(&renewed),
            2000,
        )
        .unwrap();
        assert_eq!(rotated.refresh_token, "rotated");
        assert_eq!(rotated.access_token, "new");
        assert!(parse_token(&json!({"access_token":""}), Some(&previous), 1000).is_err());
        assert!(parse_token(&json!({"access_token":"new"}), None, 1000).is_err());
    }
}
