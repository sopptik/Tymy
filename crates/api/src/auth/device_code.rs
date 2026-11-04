//! OAuth2 device authorization grant (RFC 8628) proti Microsoft identity platform.
//!
//! Pozn.: některé organizace device code flow blokují podmíněným přístupem. Pro ně
//! bude později potřeba authorization code flow s PKCE.

use std::time::Duration;

use serde::Deserialize;
use tymy_core::{Error, Result};

use super::token::{TokenResponse, TokenSet};
use crate::http::SendLogged;
use crate::{Endpoints, http::Http, http::json, http::net_err};

/// Kód, který uživatel zadá v prohlížeči.
#[derive(Debug, Clone, Deserialize)]
pub struct DeviceCode {
    pub user_code: String,
    pub verification_uri: String,
    /// Lokalizovaná instrukce od serveru, připravená k zobrazení.
    #[serde(default)]
    pub message: Option<String>,
    device_code: String,
    #[serde(default = "default_interval")]
    interval: u64,
    #[serde(default = "default_expires")]
    expires_in: u64,
}

fn default_interval() -> u64 {
    5
}
fn default_expires() -> u64 {
    900
}

#[derive(Deserialize)]
struct OAuthError {
    error: String,
    #[serde(default)]
    error_description: Option<String>,
}

pub async fn start_device_code(http: &Http, ep: &Endpoints) -> Result<DeviceCode> {
    let resp = http
        .client
        .post(ep.device_code_url())
        .form(&[
            ("client_id", ep.client_id.as_str()),
            ("scope", &ep.login_scopes()),
        ])
        .send_logged()
        .await
        .map_err(net_err)?;
    let resp = oauth_check(resp).await?;
    json(resp).await
}

/// Čeká, dokud uživatel přihlášení nedokončí (nebo kód nevyprší).
pub async fn poll_device_code(http: &Http, ep: &Endpoints, code: &DeviceCode) -> Result<TokenSet> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(code.expires_in);
    let mut interval = Duration::from_secs(code.interval.max(1));
    loop {
        tokio::time::sleep(interval).await;
        if tokio::time::Instant::now() >= deadline {
            return Err(Error::Auth("přihlašovací kód vypršel".into()));
        }
        let resp = http
            .client
            .post(ep.token_url())
            .form(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ("client_id", ep.client_id.as_str()),
                ("device_code", code.device_code.as_str()),
            ])
            .send_logged()
            .await
            .map_err(net_err)?;
        if resp.status().is_success() {
            let r: TokenResponse = json(resp).await?;
            return Ok(r.into());
        }
        let err: OAuthError = json(resp).await?;
        match err.error.as_str() {
            "authorization_pending" => {}
            "slow_down" => interval += Duration::from_secs(5),
            _ => return Err(oauth_error(err)),
        }
    }
}

/// Získá nový access token (pro `scope`) z refresh tokenu.
/// Microsoft refresh tokeny rotuje, vrácený `refresh_token` je třeba uložit.
pub async fn refresh(
    http: &Http,
    ep: &Endpoints,
    refresh_token: &str,
    scope: &str,
) -> Result<TokenSet> {
    let scope = format!("openid profile offline_access {scope}");
    let resp = http
        .client
        .post(ep.token_url())
        .form(&[
            ("grant_type", "refresh_token"),
            ("client_id", ep.client_id.as_str()),
            ("refresh_token", refresh_token),
            ("scope", &scope),
        ])
        .send_logged()
        .await
        .map_err(net_err)?;
    let resp = oauth_check(resp).await?;
    let r: TokenResponse = json(resp).await?;
    Ok(r.into())
}

async fn oauth_check(resp: reqwest::Response) -> Result<reqwest::Response> {
    if resp.status().is_success() {
        return Ok(resp);
    }
    if resp.status().as_u16() == 400 || resp.status().as_u16() == 401 {
        let err: OAuthError = json(resp).await?;
        return Err(oauth_error(err));
    }
    crate::http::check(resp).await
}

fn oauth_error(err: OAuthError) -> Error {
    let desc = err.error_description.unwrap_or_default();
    // AADSTS chybové kódy začínají řádkem "AADSTSxxxxx: popis"
    let first_line = desc.lines().next().unwrap_or_default();
    let msg = format!("{}: {first_line}", err.error);
    match err.error.as_str() {
        "invalid_grant" | "interaction_required" | "expired_token" | "authorization_declined" => {
            Error::ReauthRequired(msg)
        }
        _ => Error::Auth(msg),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn ep(server: &MockServer) -> Endpoints {
        Endpoints {
            authority: format!("{}/organizations", server.uri()),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn device_code_flow_waits_for_user() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/organizations/oauth2/v2.0/devicecode"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "user_code": "ABCD-1234",
                "device_code": "dc",
                "verification_uri": "https://microsoft.com/devicelogin",
                "interval": 1,
                "expires_in": 60
            })))
            .mount(&server)
            .await;
        // první dva pokusy: uživatel ještě nepotvrdil (test běží v reálném čase, ~3 s)
        Mock::given(method("POST"))
            .and(path("/organizations/oauth2/v2.0/token"))
            .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
                "error": "authorization_pending"
            })))
            .up_to_n_times(2)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/organizations/oauth2/v2.0/token"))
            .and(body_string_contains("device_code=dc"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "at", "refresh_token": "rt", "expires_in": 3600
            })))
            .mount(&server)
            .await;

        let http = Http::new().unwrap();
        let ep = ep(&server);
        let code = start_device_code(&http, &ep).await.unwrap();
        assert_eq!(code.user_code, "ABCD-1234");
        let tokens = poll_device_code(&http, &ep, &code).await.unwrap();
        assert_eq!(tokens.access_token, "at");
        assert_eq!(tokens.refresh_token.as_deref(), Some("rt"));
    }

    #[tokio::test]
    async fn invalid_grant_requires_reauth() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/organizations/oauth2/v2.0/token"))
            .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
                "error": "invalid_grant",
                "error_description": "AADSTS700082: The refresh token has expired.\nTrace ID: x"
            })))
            .mount(&server)
            .await;
        let err = refresh(&Http::new().unwrap(), &ep(&server), "old", "scope")
            .await
            .unwrap_err();
        match err {
            Error::ReauthRequired(m) => {
                assert!(m.contains("AADSTS700082"));
                assert!(!m.contains("Trace ID"));
            }
            e => panic!("čekal jsem ReauthRequired, je {e:?}"),
        }
    }
}
