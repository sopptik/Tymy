//! Služba přítomnosti (UPS, `…/ups/<region>/v1`): stav uživatelů.
//!
//! Jednorázový dotaz `presence/getpresence/`; živé změny chodí přes trouter
//! (`…/unifiedPresenceService`) ve stejném tvaru, viz `docs/protocol/README.md` § 4.

use serde_json::{Value, json};
use tymy_core::{Availability, Error, Presence, Result, UserId};
use url::Url;

use crate::http::{Http, SendLogged, check, json, net_err};

pub const PRESENCE_SCOPE: &str = "https://presence.teams.microsoft.com/.default";
/// Označení klienta, které služba přítomnosti vyžaduje (hodnota web klienta Teams).
const CLIENT_USER_AGENT: &str = "Teams-V2-Web";
/// Kolik uživatelů na jeden dotaz (web klient posílá desítky).
const BATCH: usize = 50;

/// Adresa UPS odvozená z middle tieru (`…/api/mt/<region>` → `…/ups/<region>/v1/`).
pub fn ups_base(middle_tier: &Url) -> Result<Url> {
    let region = middle_tier
        .path_segments()
        .and_then(|mut s| s.rfind(|p| !p.is_empty()))
        .filter(|r| r.chars().all(|c| c.is_ascii_alphanumeric()))
        .ok_or_else(|| Error::Protocol("middle tier bez regionu".into()))?;
    middle_tier
        .join(&format!("/ups/{region}/v1/"))
        .map_err(|e| Error::Protocol(e.to_string()))
}

/// Stav uživatelů podle MRI; neznámé uživatele server vynechá nebo vrátí bez stavu.
pub async fn get_presence(
    http: &Http,
    base: &Url,
    token: &str,
    users: &[UserId],
) -> Result<Vec<(UserId, Presence)>> {
    let url = base
        .join("presence/getpresence/")
        .map_err(|e| Error::Protocol(e.to_string()))?;
    let mut out = vec![];
    for chunk in users.chunks(BATCH) {
        let body: Vec<Value> = chunk
            .iter()
            .map(|u| json!({ "mri": u.as_str(), "source": "ups" }))
            .collect();
        let resp = http
            .client
            .post(url.clone())
            .bearer_auth(token)
            .json(&body)
            .send_logged()
            .await
            .map_err(net_err)?;
        let v: Value = json(check(resp).await?).await?;
        out.extend(parse_list(&v));
    }
    Ok(out)
}

/// Přihlásí odběr změn stavu `users`; server je doručuje přes trouter na
/// `<surl>unifiedPresenceService` (hned i úvodní snímek). Nahradí předchozí odběr.
pub async fn subscribe(
    http: &Http,
    base: &Url,
    token: &str,
    epid: &str,
    surl: &str,
    client_version: &str,
    users: &[UserId],
) -> Result<()> {
    let url = base
        .join(&format!("pubsub/subscriptions/{epid}"))
        .map_err(|e| Error::Protocol(e.to_string()))?;
    for (i, chunk) in users.chunks(BATCH).enumerate() {
        let add: Vec<Value> = chunk
            .iter()
            .map(|u| json!({ "mri": u.as_str(), "source": "ups" }))
            .collect();
        let body = json!({
            "trouterUri": format!("{surl}unifiedPresenceService"),
            "shouldPurgePreviousSubscriptions": i == 0,
            "subscriptionsToAdd": add,
            "subscriptionsToRemove": [],
        });
        let resp = http
            .client
            .post(url.clone())
            .bearer_auth(token)
            .header("x-ms-endpoint-id", epid)
            // bez této hlavičky server odpoví 400 „Missing mandatory header“
            .header("x-ms-client-user-agent", CLIENT_USER_AGENT)
            .header("x-ms-correlation-id", uuid::Uuid::new_v4().to_string())
            .header("x-ms-client-version", client_version)
            .json(&body)
            .send_logged()
            .await
            .map_err(net_err)?;
        check(resp).await?;
    }
    Ok(())
}

/// Tělo oznámení z trouteru (`{"presence": [...], "isSnapshot": bool}`).
pub fn parse_notification(body: &str) -> Vec<(UserId, Presence)> {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| v.get("presence").map(parse_list))
        .unwrap_or_default()
}

/// Položky `[{mri, presence: {availability, activity, calendarData}}]` (odpověď dotazu
/// i pole `presence` v oznámení z trouteru).
pub fn parse_list(v: &Value) -> Vec<(UserId, Presence)> {
    v.as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| {
            let mri = item.get("mri")?.as_str()?;
            let p = item.get("presence")?;
            let availability = Availability::parse(p.get("availability")?.as_str()?);
            let activity = p
                .get("activity")
                .and_then(Value::as_str)
                .filter(|a| Availability::parse(a) == Availability::Unknown && !a.is_empty())
                .map(str::to_owned);
            let out_of_office = p
                .pointer("/calendarData/isOutOfOffice")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            Some((
                UserId(mri.to_owned()),
                Presence {
                    availability,
                    activity,
                    out_of_office,
                },
            ))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, Request, ResponseTemplate};

    #[test]
    fn derives_ups_base() {
        let mt = Url::parse("https://teams.example/api/mt/emea").unwrap();
        assert_eq!(
            ups_base(&mt).unwrap().as_str(),
            "https://teams.example/ups/emea/v1/"
        );
        let mt = Url::parse("https://teams.example/api/mt/emea/").unwrap();
        assert_eq!(
            ups_base(&mt).unwrap().as_str(),
            "https://teams.example/ups/emea/v1/"
        );
    }

    #[test]
    fn parses_presence_items() {
        let v = json!([
            {"mri": "8:orgid:aaaa", "source": "ups", "status": 20000, "presence": {
                "availability": "Busy", "activity": "InACall",
                "calendarData": {"isOutOfOffice": true}}},
            {"mri": "8:orgid:bbbb", "presence": {"availability": "Away", "activity": "Away"}},
            {"mri": "8:orgid:cccc", "status": 40400},
        ]);
        let list = parse_list(&v);
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].0.as_str(), "8:orgid:aaaa");
        assert_eq!(list[0].1.availability, Availability::Busy);
        assert_eq!(list[0].1.activity.as_deref(), Some("InACall"));
        assert!(list[0].1.out_of_office);
        assert_eq!(list[1].1.availability, Availability::Away);
        assert_eq!(list[1].1.activity, None);
    }

    #[test]
    fn parses_trouter_notification() {
        let body = r#"{"presence": [{"mri": "8:orgid:aaaa", "presence":
            {"availability": "DoNotDisturb", "activity": "Presenting"}}], "isSnapshot": true}"#;
        let list = parse_notification(body);
        assert_eq!(list[0].1.availability, Availability::DoNotDisturb);
        assert_eq!(list[0].1.activity.as_deref(), Some("Presenting"));
        assert!(parse_notification("nesmysl").is_empty());
    }

    #[tokio::test]
    async fn subscribes_with_trouter_uri() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/ups/emea/v1/pubsub/subscriptions/ep-1"))
            .and(header("x-ms-endpoint-id", "ep-1"))
            .respond_with(|req: &Request| {
                let body: Value = serde_json::from_slice(&req.body).unwrap();
                assert_eq!(
                    body["trouterUri"],
                    "https://pub.example/v4/f/abc/unifiedPresenceService"
                );
                assert_eq!(body["shouldPurgePreviousSubscriptions"], true);
                assert_eq!(body["subscriptionsToAdd"][0]["mri"], "8:orgid:aaaa");
                ResponseTemplate::new(200).set_body_json(json!({"addStatuses": []}))
            })
            .expect(1)
            .mount(&server)
            .await;
        let base = Url::parse(&format!("{}/ups/emea/v1/", server.uri())).unwrap();
        subscribe(
            &Http::new().unwrap(),
            &base,
            "t",
            "ep-1",
            "https://pub.example/v4/f/abc/",
            "1415/1",
            &[UserId("8:orgid:aaaa".into())],
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn queries_presence() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/ups/emea/v1/presence/getpresence/"))
            .and(header("Authorization", "Bearer t"))
            .respond_with(|req: &Request| {
                let body: Value = serde_json::from_slice(&req.body).unwrap();
                assert_eq!(body, json!([{"mri": "8:orgid:aaaa", "source": "ups"}]));
                ResponseTemplate::new(200).set_body_json(json!([
                    {"mri": "8:orgid:aaaa", "presence": {"availability": "Available", "activity": "Available"}}
                ]))
            })
            .expect(1)
            .mount(&server)
            .await;
        let base = Url::parse(&format!("{}/ups/emea/v1/", server.uri())).unwrap();
        let list = get_presence(
            &Http::new().unwrap(),
            &base,
            "t",
            &[UserId("8:orgid:aaaa".into())],
        )
        .await
        .unwrap();
        assert_eq!(list[0].1.availability, Availability::Available);
    }
}
