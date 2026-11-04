//! Stahování médií vložených do zpráv (AMS, Async Media Service).

use serde_json::{Value, json};
use tymy_core::{ConversationId, Error, Result};
use url::Url;

use crate::auth::SkypeSession;
use crate::http::{Http, SendLogged, check, json as read_json, net_err};

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
        .send_logged()
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

/// Adresa media služby pro region chat služby. Pozorováno: EMEA → `eu-api.asm.skype.com`.
/// Ostatní regiony NEOVĚŘENO.
pub(crate) fn ams_base(session: &SkypeSession) -> Url {
    let host = session.chat_service.host_str().unwrap_or_default();
    let region = host.split('.').next().unwrap_or_default();
    let prefix = match region {
        "emea" | "eu" => "eu",
        "apac" | "jp" | "au" | "in" => "apac",
        _ => "us",
    };
    if cfg!(test) && host == "127.0.0.1" {
        return session.chat_service.clone();
    }
    Url::parse(&format!("https://{prefix}-api.asm.skype.com/")).expect("platná URL")
}

/// Nahraje obrázek do media služby a vrátí id objektu (`0-weu-d1-…`).
///
/// Postup (NEOVĚŘENO pro zápis, převzato z protokolu Skype):
/// 1. `POST /v1/objects` `{type: "pish/image", permissions: {<chat>: ["read"]}, filename}` → `{id}`
/// 2. `PUT /v1/objects/<id>/content/imgpsh` s daty obrázku
pub async fn upload_image(
    http: &Http,
    session: &SkypeSession,
    conversation: &ConversationId,
    bytes: &[u8],
    filename: &str,
) -> Result<String> {
    if bytes.len() as u64 > MAX_BYTES {
        return Err(Error::Other("obrázek je příliš velký (max. 25 MB)".into()));
    }
    let base = ams_base(session);
    let auth = format!("skype_token {}", session.token);
    let url = base.join("v1/objects").expect("platná cesta");
    let body = json!({
        "type": "pish/image",
        "permissions": { conversation.as_str(): ["read"] },
        "filename": filename,
    });
    let resp = http
        .client
        .post(url)
        .header(reqwest::header::AUTHORIZATION, &auth)
        .json(&body)
        .send_logged()
        .await
        .map_err(net_err)?;
    let v: Value = read_json(check(resp).await?).await?;
    let id = v
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty() && !id.contains('/'))
        .ok_or_else(|| Error::Protocol("media služba nevrátila id objektu".into()))?
        .to_owned();
    let mut url = base.clone();
    url.path_segments_mut()
        .expect("https URL")
        .extend(["v1", "objects", &id, "content", "imgpsh"]);
    let resp = http
        .client
        .put(url)
        .header(reqwest::header::AUTHORIZATION, &auth)
        .header(reqwest::header::CONTENT_TYPE, "application/octet-stream")
        .body(bytes.to_vec())
        .send_logged()
        .await
        .map_err(net_err)?;
    check(resp).await?;
    tracing::debug!(%id, bytes = bytes.len(), "obrázek nahrán");
    Ok(id)
}

/// HTML značka obrázku pro zprávu (stejný tvar, jaký posílá web klient).
pub fn image_html(session: &SkypeSession, id: &str, width: u32, height: u32) -> String {
    let src = ams_base(session)
        .join(&format!("v1/objects/{id}/views/imgo"))
        .expect("platná cesta");
    format!(
        r#"<p><img src="{src}" itemscope="png" itemtype="http://schema.skype.com/AMSImage" width="{width}" height="{height}" alt="obrázek" id="x_{id}" itemid="{id}"></p>"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, Utc};
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn session_at(chat: &str) -> SkypeSession {
        SkypeSession {
            token: "st".into(),
            expires_at: Utc::now() + Duration::hours(1),
            chat_service: chat.parse().unwrap(),
            bearer: String::new(),
            middle_tier: None,
        }
    }

    #[test]
    fn ams_host_by_region() {
        assert_eq!(
            ams_base(&session_at("https://emea.ng.msg.teams.microsoft.com")).as_str(),
            "https://eu-api.asm.skype.com/"
        );
        assert_eq!(
            ams_base(&session_at("https://amer.ng.msg.teams.microsoft.com")).as_str(),
            "https://us-api.asm.skype.com/"
        );
        let html = image_html(
            &session_at("https://emea.ng.msg.teams.microsoft.com"),
            "0-weu-d1-abc",
            300,
            200,
        );
        // vlastní HTML musí naše čtení zpráv poznat jako obrázek
        let parts = tymy_core::text::message_parts(&html);
        match &parts[..] {
            [tymy_core::text::Part::Image(i)] => {
                assert_eq!(
                    i.url,
                    "https://eu-api.asm.skype.com/v1/objects/0-weu-d1-abc/views/imgo"
                );
                assert_eq!(i.width, Some(300.0));
            }
            p => panic!("{p:?}"),
        }
    }

    #[tokio::test]
    async fn uploads_image_in_two_steps() {
        use wiremock::matchers::body_string_contains;
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/objects"))
            .and(header("Authorization", "skype_token st"))
            .and(body_string_contains("\"19:c@thread.v2\":[\"read\"]"))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({"id": "0-weu-d1-new"})))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("PUT"))
            .and(path("/v1/objects/0-weu-d1-new/content/imgpsh"))
            .and(header("Authorization", "skype_token st"))
            .respond_with(ResponseTemplate::new(201))
            .expect(1)
            .mount(&server)
            .await;
        let s = session_at(&server.uri());
        let id = upload_image(
            &Http::new().unwrap(),
            &s,
            &"19:c@thread.v2".into(),
            b"PNG",
            "a.png",
        )
        .await
        .unwrap();
        assert_eq!(id, "0-weu-d1-new");
    }

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
            bearer: String::new(),
            middle_tier: None,
        };
        let url = format!("{}/v1/objects/o1/views/imgo", server.uri());
        assert_eq!(
            fetch(&Http::new().unwrap(), &s, &url).await.unwrap(),
            b"PNG"
        );
    }
}
