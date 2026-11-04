//! Sdílený HTTP klient, mapování chyb a bezpečné logování.

use std::time::Duration;

use reqwest::{Response, StatusCode, header};
use tymy_core::{Error, Result};

const USER_AGENT: &str = concat!("tymy/", env!("CARGO_PKG_VERSION"), " (Linux)");
/// Maximální délka těla chybové odpovědi, která se přenese do chybové hlášky.
const MAX_ERROR_BODY: usize = 2000;

#[derive(Debug, Clone)]
pub struct Http {
    pub(crate) client: reqwest::Client,
}

impl Http {
    pub fn new() -> Result<Self> {
        let client = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .connect_timeout(Duration::from_secs(15))
            // Long-poll si nastavuje vlastní delší timeout na požadavku.
            .timeout(Duration::from_secs(45))
            // Přesměrování neřešíme automaticky: nechceme poslat token na nečekaný host.
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| Error::Network(e.to_string()))?;
        Ok(Self { client })
    }
}

pub(crate) fn net_err(e: reqwest::Error) -> Error {
    if e.is_decode() {
        Error::Protocol(e.to_string())
    } else {
        Error::Network(e.to_string())
    }
}

/// Převede neúspěšnou odpověď na chybu, úspěšnou vrátí beze změny.
pub(crate) async fn check(resp: Response) -> Result<Response> {
    let status = resp.status();
    if status.is_success() {
        return Ok(resp);
    }
    let retry_after = resp
        .headers()
        .get(header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map(Duration::from_secs);
    let url = resp.url().clone();
    let body = resp.text().await.unwrap_or_default();
    let body = redact(&truncate(&body, MAX_ERROR_BODY));
    tracing::debug!(%status, %url, %body, "neúspěšná odpověď");
    Err(match status {
        StatusCode::TOO_MANY_REQUESTS => Error::RateLimited { retry_after },
        StatusCode::UNAUTHORIZED => Error::Auth(format!("HTTP 401: {body}")),
        _ => Error::Http {
            status: status.as_u16(),
            body,
        },
    })
}

/// Načte tělo jako JSON; při chybě parsování zaloguje (zredigované) tělo kvůli ladění protokolu.
pub(crate) async fn json<T: serde::de::DeserializeOwned>(resp: Response) -> Result<T> {
    let url = resp.url().clone();
    let text = resp.text().await.map_err(net_err)?;
    tracing::trace!(%url, body = %redact(&text), "odpověď");
    serde_json::from_str(&text).map_err(|e| {
        tracing::warn!(%url, error = %e, body = %redact(&truncate(&text, MAX_ERROR_BODY)), "nečekaný JSON");
        Error::Protocol(format!("{url}: {e}"))
    })
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_owned();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

/// Nahradí v textu věci, které vypadají jako tokeny (JWT a dlouhé base64 řetězce),
/// aby se nedostaly do logů ani dumpů.
pub fn redact(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut token = String::new();
    let flush = |token: &mut String, out: &mut String| {
        if looks_like_secret(token) {
            out.push_str("<redacted>");
        } else {
            out.push_str(token);
        }
        token.clear();
    };
    for c in s.chars() {
        if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '+' | '/' | '=' | '%') {
            token.push(c);
        } else {
            flush(&mut token, &mut out);
            out.push(c);
        }
    }
    flush(&mut token, &mut out);
    out
}

fn looks_like_secret(t: &str) -> bool {
    // JWT (header začíná `{"` → base64 `eyJ`), nebo jakýkoli velmi dlouhý neprostorový řetězec
    // (refresh tokeny, skypetokeny, registration tokeny). URL s lomítky mají jinou strukturu,
    // proto u dlouhých řetězců vyžadujeme malý podíl lomítek.
    (t.starts_with("eyJ") && t.len() > 40)
        || (t.len() > 120 && t.matches('/').count() * 20 < t.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_jwt_and_long_tokens() {
        let jwt = format!("eyJ{}", "a".repeat(60));
        let long = "x".repeat(200);
        let s = format!(r#"{{"skypeToken":"{jwt}","refresh_token":"{long}","name":"Alice"}}"#);
        let r = redact(&s);
        assert!(!r.contains(&jwt));
        assert!(!r.contains(&long));
        assert!(r.contains("Alice"));
        assert!(r.contains("skypeToken"));
    }

    #[test]
    fn keeps_urls() {
        let url = "https://emea.ng.msg.teams.microsoft.com/v1/users/ME/conversations/19:abc@thread.v2/messages";
        assert_eq!(redact(url), url);
    }

    #[test]
    fn truncate_respects_char_boundary() {
        assert_eq!(truncate("žžž", 3), "ž…");
    }
}
