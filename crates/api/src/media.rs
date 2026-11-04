//! Stahování médií vložených do zpráv (AMS, Async Media Service).

use tymy_core::{Error, Result};
use url::Url;

use crate::auth::SkypeSession;
use crate::http::{Http, check, net_err};

/// Horní mez velikosti staženého média.
const MAX_BYTES: u64 = 25 * 1024 * 1024;

/// Skypetoken posíláme jen na hosty media služby.
pub(crate) fn validate_url(url: &str) -> Result<Url> {
    let u = Url::parse(url).map_err(|e| Error::Protocol(format!("URL média {url}: {e}")))?;
    let allowed =
        u.scheme() == "https" && u.host_str().is_some_and(|h| h.ends_with(".asm.skype.com"));
    // V testech povolujeme mock server na loopbacku.
    let test_loopback = cfg!(test) && u.host_str() == Some("127.0.0.1");
    if !allowed && !test_loopback {
        return Err(Error::Protocol(format!(
            "médium mimo media službu: {}",
            u.host_str().unwrap_or("?")
        )));
    }
    Ok(u)
}

pub async fn fetch(http: &Http, session: &SkypeSession, url: &str) -> Result<Vec<u8>> {
    let url = validate_url(url)?;
    let resp = http
        .client
        .get(url)
        .header(
            reqwest::header::AUTHORIZATION,
            format!("skype_token {}", session.token),
        )
        .send()
        .await
        .map_err(net_err)?;
    let resp = check(resp).await?;
    if resp.content_length().is_some_and(|l| l > MAX_BYTES) {
        return Err(Error::Protocol("médium je příliš velké".into()));
    }
    let bytes = resp.bytes().await.map_err(net_err)?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err(Error::Protocol("médium je příliš velké".into()));
    }
    Ok(bytes.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, Utc};
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn only_media_hosts_get_the_token() {
        assert!(validate_url("https://eu-api.asm.skype.com/v1/objects/x/views/imgo").is_ok());
        assert!(validate_url("https://evil.example/x").is_err());
        assert!(validate_url("https://asm.skype.com.evil.example/x").is_err());
        assert!(validate_url("http://eu-api.asm.skype.com/x").is_err());
    }

    #[tokio::test]
    async fn fetches_with_skype_token_header() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/objects/o1/views/imgo"))
            .and(header("Authorization", "skype_token st"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"PNG".to_vec()))
            .mount(&server)
            .await;
        let s = SkypeSession {
            token: "st".into(),
            expires_at: Utc::now() + Duration::hours(1),
            chat_service: server.uri().parse().unwrap(),
        };
        let url = format!("{}/v1/objects/o1/views/imgo", server.uri());
        assert_eq!(
            fetch(&Http::new().unwrap(), &s, &url).await.unwrap(),
            b"PNG"
        );
    }
}
