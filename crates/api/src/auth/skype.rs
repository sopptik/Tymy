//! Výměna OAuth access tokenu za skypetoken (authsvc).

use chrono::{DateTime, Duration, Utc};
use serde_json::Value;
use tymy_core::{Error, Result};
use url::Url;

use crate::{Endpoints, http::Http, http::check, http::json, http::net_err};

/// Platná relace vůči chat službě.
#[derive(Clone)]
pub struct SkypeSession {
    pub token: String,
    pub expires_at: DateTime<Utc>,
    /// Regionální chat služba, např. `https://emea.ng.msg.teams.microsoft.com`.
    pub chat_service: Url,
}

impl std::fmt::Debug for SkypeSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SkypeSession")
            .field("token", &"<redacted>")
            .field("expires_at", &self.expires_at)
            .field("chat_service", &self.chat_service.as_str())
            .finish()
    }
}

impl SkypeSession {
    /// Je token platný ještě alespoň `margin`?
    pub fn valid_for(&self, margin: Duration) -> bool {
        self.expires_at - margin > Utc::now()
    }
}

pub async fn exchange_for_skype_token(
    http: &Http,
    ep: &Endpoints,
    access_token: &str,
) -> Result<SkypeSession> {
    let resp = http
        .client
        .post(&ep.authz_url)
        .bearer_auth(access_token)
        .header(reqwest::header::CONTENT_LENGTH, 0)
        .send()
        .await
        .map_err(net_err)?;
    let body: Value = json(check(resp).await?).await?;
    parse_authz(&body, &ep.chat_service)
}

/// Formát odpovědi není dokumentovaný a liší se mezi pracovními a osobními účty,
/// proto parsujeme tolerantně: token i adresu služby hledáme podle názvu klíče.
pub(crate) fn parse_authz(body: &Value, default_chat_service: &str) -> Result<SkypeSession> {
    let token = find_string(body, &["skypeToken", "skypetoken"])
        .ok_or_else(|| Error::Protocol("authz odpověď neobsahuje skypeToken".into()))?;
    let expires_in = find_value(body, &["expiresIn", "expires_in"])
        .and_then(|v| v.as_i64().or_else(|| v.as_str()?.parse().ok()))
        .unwrap_or(3600);
    let chat_service = body
        .get("regionGtms")
        .and_then(|g| g.get("chatService"))
        .and_then(Value::as_str)
        .unwrap_or(default_chat_service);
    let chat_service = Url::parse(chat_service)
        .map_err(|e| Error::Protocol(format!("chatService URL {chat_service}: {e}")))?;
    // V testech povolujeme http na loopbacku kvůli mock serveru.
    let test_loopback = cfg!(test) && chat_service.host_str() == Some("127.0.0.1");
    if chat_service.scheme() != "https" && !test_loopback {
        return Err(Error::Protocol(format!(
            "chatService musí být https, je {chat_service}"
        )));
    }
    Ok(SkypeSession {
        token: token.to_owned(),
        expires_at: Utc::now() + Duration::seconds(expires_in),
        chat_service,
    })
}

fn find_value<'a>(v: &'a Value, keys: &[&str]) -> Option<&'a Value> {
    match v {
        Value::Object(map) => {
            for k in keys {
                if let Some(found) = map.get(*k)
                    && !found.is_object()
                {
                    return Some(found);
                }
            }
            map.values().find_map(|child| find_value(child, keys))
        }
        Value::Array(items) => items.iter().find_map(|child| find_value(child, keys)),
        _ => None,
    }
}

fn find_string<'a>(v: &'a Value, keys: &[&str]) -> Option<&'a str> {
    find_value(v, keys)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_work_response() {
        let body = json!({
            "tokens": { "skypeToken": "st", "expiresIn": 86399, "tokenType": "SkypeToken" },
            "region": "emea",
            "regionGtms": { "chatService": "https://emea.ng.msg.teams.microsoft.com", "middleTier": "https://x" }
        });
        let s = parse_authz(&body, "https://default.example").unwrap();
        assert_eq!(s.token, "st");
        assert_eq!(
            s.chat_service.as_str(),
            "https://emea.ng.msg.teams.microsoft.com/"
        );
        assert!(s.valid_for(Duration::hours(23)));
    }

    #[test]
    fn parses_nested_lowercase_and_uses_default_service() {
        let body = json!({ "skypeToken": { "skypetoken": "st2", "expiresIn": "600" } });
        let s = parse_authz(&body, "https://msgapi.teams.live.com").unwrap();
        assert_eq!(s.token, "st2");
        assert_eq!(s.chat_service.host_str(), Some("msgapi.teams.live.com"));
        assert!(!s.valid_for(Duration::minutes(11)));
    }

    #[test]
    fn rejects_missing_token_and_http_service() {
        assert!(parse_authz(&json!({"tokens": {}}), "https://x").is_err());
        let body =
            json!({"tokens": {"skypeToken": "t"}, "regionGtms": {"chatService": "http://evil"}});
        assert!(parse_authz(&body, "https://x").is_err());
    }
}
