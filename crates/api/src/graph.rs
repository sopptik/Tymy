//! Microsoft Graph: plánování schůzek v kalendáři (vytvoření, zrušení, odpověď na pozvánku).
//!
//! Čtení kalendáře jde přes middle tier Teams ([`crate::calendar`]); zápis přes Graph, protože
//! je zdokumentovaný a jedním voláním vytvoří i Teams schůzku (`isOnlineMeeting`).
//! Token pro `https://graph.microsoft.com/.default` vydá stejné přihlášení (Teams desktop
//! client id) s rozsahem `Calendars.ReadWrite` (ověřeno 2026-11-09). Web klient Teams používá
//! místo toho Outlook (`owa/service.svc` `CreateCalendarEvent` / `CancelCalendarEvent`
//! a GraphQL `createOnlineMeeting`), viz `docs/protocol/README.md`.

use chrono::{DateTime, Utc};
use reqwest::Method;
use serde_json::{Value, json};
use tymy_core::{Error, NewMeeting, Result};

use crate::http::{Http, SendLogged, check, json, net_err};

pub const GRAPH_SCOPE: &str = "https://graph.microsoft.com/.default";
const GRAPH_BASE: &str = "https://graph.microsoft.com";

/// Požadavek na Graph; `path` začíná `/v1.0/` nebo `/beta/`.
pub async fn request(
    http: &Http,
    token: &str,
    method: Method,
    path: &str,
    body: Option<&Value>,
) -> Result<Value> {
    request_at(http, GRAPH_BASE, token, method, path, body).await
}

async fn request_at(
    http: &Http,
    base: &str,
    token: &str,
    method: Method,
    path: &str,
    body: Option<&Value>,
) -> Result<Value> {
    if !(path.starts_with("/v1.0/") || path.starts_with("/beta/")) {
        return Err(Error::Other(format!(
            "cesta Graph musí začínat /v1.0/: {path}"
        )));
    }
    let mut rb = http
        .client
        .request(method, format!("{base}{path}"))
        .bearer_auth(token);
    rb = match body {
        Some(b) => rb.json(b),
        None => rb.header(reqwest::header::CONTENT_LENGTH, 0),
    };
    let resp = check(rb.send_logged().await.map_err(net_err)?).await?;
    if resp.status() == reqwest::StatusCode::NO_CONTENT || resp.content_length() == Some(0) {
        return Ok(Value::Null);
    }
    json(resp).await
}

/// Tělo nové události pro `POST /v1.0/me/events`. Časy posíláme v UTC.
pub fn event_body(m: &NewMeeting) -> Value {
    let time = |t: DateTime<Utc>| json!({ "dateTime": t.format("%Y-%m-%dT%H:%M:%S").to_string(), "timeZone": "UTC" });
    let attendees: Vec<Value> = m
        .attendees
        .iter()
        .map(|a| {
            json!({
                "type": "required",
                "emailAddress": { "address": a.email, "name": a.name },
            })
        })
        .collect();
    let mut body = json!({
        "subject": m.subject.trim(),
        "start": time(m.start),
        "end": time(m.end),
        "attendees": attendees,
        "isReminderOn": true,
        "reminderMinutesBeforeStart": m.reminder_minutes,
    });
    if m.online {
        body["isOnlineMeeting"] = json!(true);
        body["onlineMeetingProvider"] = json!("teamsForBusiness");
    }
    if let Some(text) = m.body.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
        body["body"] = json!({ "contentType": "text", "content": text });
    }
    if let Some(place) = m
        .location
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
    {
        body["location"] = json!({ "displayName": place });
    }
    body
}

/// Id události z odpovědi Graphu.
pub fn created_id(v: &Value) -> Result<String> {
    v.get("id")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| Error::Protocol("Graph: vytvořená událost bez id".into()))
}

/// Cesta k události (id je base64 s `/`, `+`, `=` → zakódovat).
pub fn event_path(id: &str, suffix: &str) -> String {
    let enc: String = url::form_urlencoded::byte_serialize(id.as_bytes()).collect();
    format!("/v1.0/me/events/{enc}{suffix}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use tymy_core::MeetingAttendee;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, Request, ResponseTemplate};

    fn meeting() -> NewMeeting {
        NewMeeting {
            subject: " Porada ".into(),
            start: Utc.with_ymd_and_hms(2026, 11, 10, 9, 0, 0).unwrap(),
            end: Utc.with_ymd_and_hms(2026, 11, 10, 9, 30, 0).unwrap(),
            attendees: vec![MeetingAttendee {
                email: "jan@firma.cz".into(),
                name: "Jan Novák".into(),
            }],
            online: true,
            body: Some("Program".into()),
            location: None,
            reminder_minutes: 15,
        }
    }

    #[test]
    fn builds_event_body() {
        let b = event_body(&meeting());
        assert_eq!(b["subject"], "Porada");
        assert_eq!(
            b["start"],
            json!({"dateTime": "2026-11-10T09:00:00", "timeZone": "UTC"})
        );
        assert_eq!(b["end"]["dateTime"], "2026-11-10T09:30:00");
        assert_eq!(b["isOnlineMeeting"], true);
        assert_eq!(b["onlineMeetingProvider"], "teamsForBusiness");
        assert_eq!(b["attendees"][0]["emailAddress"]["address"], "jan@firma.cz");
        assert_eq!(b["attendees"][0]["type"], "required");
        assert_eq!(b["body"]["content"], "Program");
        assert!(b.get("location").is_none());
        let mut offline = meeting();
        offline.online = false;
        assert!(event_body(&offline).get("isOnlineMeeting").is_none());
    }

    #[test]
    fn encodes_event_ids() {
        assert_eq!(
            event_path("AAMk/a+b=", "/cancel"),
            "/v1.0/me/events/AAMk%2Fa%2Bb%3D/cancel"
        );
    }

    #[tokio::test]
    async fn posts_with_bearer_and_rejects_foreign_paths() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1.0/me/events"))
            .and(header("Authorization", "Bearer t"))
            .respond_with(|req: &Request| {
                let body: Value = serde_json::from_slice(&req.body).unwrap();
                assert_eq!(body["subject"], "Porada");
                ResponseTemplate::new(201).set_body_json(json!({"id": "AAMkX"}))
            })
            .expect(1)
            .mount(&server)
            .await;
        let http = Http::new().unwrap();
        let v = request_at(
            &http,
            &server.uri(),
            "t",
            Method::POST,
            "/v1.0/me/events",
            Some(&event_body(&meeting())),
        )
        .await
        .unwrap();
        assert_eq!(created_id(&v).unwrap(), "AAMkX");
        assert!(
            request_at(&http, &server.uri(), "t", Method::GET, "https://evil", None)
                .await
                .is_err()
        );
    }
}
