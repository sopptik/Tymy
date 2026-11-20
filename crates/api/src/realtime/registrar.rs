//! Registrar: ohlášení adresy trouteru, na kterou má server doručovat oznámení o zprávách.
//!
//! Bez registrace chodí přes trouter jen to, co si klient objedná sám (přítomnost).
//! Registrace platí `ttl` sekund a je třeba ji obnovovat.

use serde_json::{Value, json};
use tymy_core::Result;

use crate::http::{Http, SendLogged, check, net_err};

/// Platnost registrace v sekundách (web klient posílá 3600).
pub const TTL_SECS: u64 = 3600;

/// Šablona registrace: `appId` a `templateKey` určují, jaká oznámení a v jakém tvaru server
/// posílá. Web klient Teams registruje dvě (každou pro vlastní spojení trouteru).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Template {
    /// Zprávy a „píše…“ (`TeamsCDLWebWorker`).
    Chat,
    /// Příchozí hovory (`SkypeSpacesWeb`).
    Calls,
}

impl Template {
    /// `(appId, templateKey, platform)` jako v HAR web klienta.
    fn ids(self) -> (&'static str, &'static str, &'static str) {
        match self {
            Template::Chat => ("TeamsCDLWebWorker", "TeamsCDLWebWorker_2.7", "edge"),
            Template::Calls => ("SkypeSpacesWeb", "SkypeSpacesWeb_2.6", "edgeChromium"),
        }
    }
}

/// Tělo registrace adresy `surl` spojení `epid`.
pub fn body(template: Template, epid: &str, surl: &str, client_version: &str) -> Value {
    let (app_id, template_key, platform) = template.ids();
    json!({
        "clientDescription": {
            "appId": app_id,
            "aesKey": "",
            "languageId": "en-US",
            "platform": platform,
            "templateKey": template_key,
            "platformUIVersion": client_version,
        },
        "registrationId": epid,
        "nodeId": "",
        "transports": { "TROUTER": [{ "context": "", "path": surl, "ttl": TTL_SECS }] },
    })
}

pub async fn register(
    http: &Http,
    template: Template,
    url: &str,
    skypetoken: &str,
    epid: &str,
    surl: &str,
    client_version: &str,
) -> Result<()> {
    let resp = http
        .client
        .post(url)
        .header("X-Skypetoken", skypetoken)
        .json(&body(template, epid, surl, client_version))
        .send_logged()
        .await
        .map_err(net_err)?;
    check(resp).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, Request, ResponseTemplate};

    #[tokio::test]
    async fn registers_trouter_path() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/registrar/prod/V2/registrations"))
            .and(header("X-Skypetoken", "st"))
            .respond_with(|req: &Request| {
                let b: Value = serde_json::from_slice(&req.body).unwrap();
                assert_eq!(b["registrationId"], "ep-1");
                assert_eq!(
                    b["clientDescription"]["templateKey"],
                    "TeamsCDLWebWorker_2.7"
                );
                assert_eq!(
                    b["transports"]["TROUTER"][0]["path"],
                    "https://pub.example/v4/f/abc/"
                );
                assert_eq!(b["transports"]["TROUTER"][0]["ttl"], 3600);
                ResponseTemplate::new(202)
            })
            .expect(1)
            .mount(&server)
            .await;
        let url = format!("{}/registrar/prod/V2/registrations", server.uri());
        register(
            &Http::new().unwrap(),
            Template::Chat,
            &url,
            "st",
            "ep-1",
            "https://pub.example/v4/f/abc/",
            "1415/1",
        )
        .await
        .unwrap();
    }

    #[test]
    fn calls_template_matches_web_client() {
        let b = body(
            Template::Calls,
            "ep-2",
            "https://pub.example/v4/f/def/",
            "1415/1",
        );
        assert_eq!(b["clientDescription"]["appId"], "SkypeSpacesWeb");
        assert_eq!(b["clientDescription"]["templateKey"], "SkypeSpacesWeb_2.6");
        assert_eq!(b["registrationId"], "ep-2");
    }
}
