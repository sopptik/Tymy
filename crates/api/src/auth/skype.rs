//! Výměna OAuth access tokenu za skypetoken (authsvc).

use chrono::{DateTime, Duration, Utc};
use serde_json::Value;
use tymy_core::{Error, Result};
use url::Url;

use crate::http::SendLogged;
use crate::{Endpoints, http::Http, http::check, http::json, http::net_err};

/// Platná relace vůči chat službě.
#[derive(Clone)]
pub struct SkypeSession {
    pub token: String,
    pub expires_at: DateTime<Utc>,
    /// Regionální chat služba, např. `https://emea.ng.msg.teams.microsoft.com`.
    pub chat_service: Url,
    /// OAuth access token (scope `skype_scope`), kterým se autorizuje middle tier.
    pub bearer: String,
    /// Middle tier (profily, hledání lidí), např. `https://teams.microsoft.com/api/mt/emea`.
    pub middle_tier: Option<Url>,
    /// Další regionální služby z authz odpovědi (chybí-li, platí výchozí z [`Endpoints`]).
    pub services: RegionServices,
}

/// Regionální adresy služeb z `regionGtms`. Posílají se na ně tokeny, proto se berou
/// jen `https` adresy na doménách Microsoftu.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RegionServices {
    /// WebSocket trouteru, `wss://…/v4/c` (z `calling_trouterUrl`).
    pub trouter: Option<String>,
    /// Registrar (`calling_registrarUrl`).
    pub registrar: Option<String>,
    /// Služba přítomnosti, `…/ups/<region>/v1/` (z `presenceUPS`).
    pub ups: Option<Url>,
    /// Založení hovoru (`calling_conversationServiceUrl`).
    pub call_service: Option<String>,
}

/// `https` adresa na doméně Microsoftu?
pub(crate) fn trusted(u: &Url) -> bool {
    const DOMAINS: [&str; 4] = ["microsoft.com", "cloud.microsoft", "skype.com", "live.com"];
    u.scheme() == "https"
        && u.host_str().is_some_and(|h| {
            DOMAINS
                .iter()
                .any(|d| h == *d || h.ends_with(&format!(".{d}")))
        })
}

impl RegionServices {
    fn parse(gtms: &Value) -> Self {
        let url = |key: &str| {
            gtms.get(key)
                .and_then(Value::as_str)
                .and_then(|u| Url::parse(u).ok())
                .filter(trusted)
        };
        Self {
            // authz uvádí `https://…/v3/c`; web klient se připojuje na `wss://…/v4/c`
            trouter: url("calling_trouterUrl")
                .and_then(|u| Some(format!("wss://{}/v4/c", u.host_str()?))),
            registrar: url("calling_registrarUrl").map(String::from),
            ups: url("presenceUPS").and_then(|u| {
                Url::parse(&format!("{}/v1/", u.as_str().trim_end_matches('/'))).ok()
            }),
            call_service: url("calling_conversationServiceUrl").map(String::from),
        }
    }
}

impl std::fmt::Debug for SkypeSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SkypeSession")
            .field("token", &"<redacted>")
            .field("expires_at", &self.expires_at)
            .field("chat_service", &self.chat_service.as_str())
            .field("middle_tier", &self.middle_tier.as_ref().map(Url::as_str))
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
        .send_logged()
        .await
        .map_err(net_err)?;
    let body: Value = json(check(resp).await?).await?;
    let mut session = parse_authz(&body, &ep.chat_service)?;
    session.bearer = access_token.to_owned();
    Ok(session)
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
    let middle_tier = body
        .get("regionGtms")
        .and_then(|g| g.get("middleTier"))
        .and_then(Value::as_str)
        .and_then(|u| Url::parse(u).ok())
        .filter(|u| u.scheme() == "https");
    Ok(SkypeSession {
        token: token.to_owned(),
        expires_at: Utc::now() + Duration::seconds(expires_in),
        chat_service,
        bearer: String::new(),
        middle_tier,
        services: body
            .get("regionGtms")
            .map(RegionServices::parse)
            .unwrap_or_default(),
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
        assert_eq!(s.middle_tier.as_ref().unwrap().as_str(), "https://x/");
    }

    #[test]
    fn reads_regional_services_only_from_microsoft_hosts() {
        let body = json!({
            "tokens": { "skypeToken": "st" },
            "regionGtms": {
                "chatService": "https://amer.ng.msg.teams.microsoft.com",
                "calling_trouterUrl": "https://go-us.trouter.teams.microsoft.com/v3/c",
                "calling_registrarUrl": "https://teams.microsoft.com/registrar/prod/V2/registrations",
                "presenceUPS": "https://teams.microsoft.com/ups/amer",
                "calling_conversationServiceUrl": "https://evil.example/api/v2/epconv"
            }
        });
        let s = parse_authz(&body, "https://x").unwrap().services;
        assert_eq!(
            s.trouter.as_deref(),
            Some("wss://go-us.trouter.teams.microsoft.com/v4/c")
        );
        assert_eq!(
            s.registrar.as_deref(),
            Some("https://teams.microsoft.com/registrar/prod/V2/registrations")
        );
        assert_eq!(
            s.ups.unwrap().as_str(),
            "https://teams.microsoft.com/ups/amer/v1/"
        );
        assert_eq!(s.call_service, None, "cizí hostitel se nepoužije");
        let plain = parse_authz(&json!({"tokens": {"skypeToken": "t"}}), "https://x").unwrap();
        assert_eq!(plain.services, RegionServices::default());
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
