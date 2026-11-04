//! HTTP long-poll (NEOVĚŘENO proti produkci, viz `docs/protocol/README.md`).

use std::time::Duration;

use reqwest::RequestBuilder;
use serde_json::json;
use tymy_core::{Event, Result};

use crate::auth::SkypeSession;
use crate::chat::wire::PollResponse;
use crate::chat::{api_url, authed};
use crate::http::{Http, check, json, net_err};

/// Jak dlouho čekáme na odpověď long-pollu (server ji drží typicky desítky sekund).
const POLL_TIMEOUT: Duration = Duration::from_secs(90);

const ENDPOINT_FEATURES: &str = "Agent,Presence2015,MessageProperties,CustomUserProperties,NotificationStream,SupportsSkipRosterFromThreads";

const INTERESTED_RESOURCES: &[&str] = &[
    "/v1/users/ME/conversations/ALL/properties",
    "/v1/users/ME/conversations/ALL/messages",
    "/v1/threads/ALL",
];

/// Zaregistrovaný endpoint + subscription.
#[derive(Clone, Default)]
pub(crate) struct PollRegistration {
    /// Hodnota pro hlavičku `RegistrationToken`, pokud ji server vydal.
    registration_token: Option<String>,
}

fn with_registration(
    rb: RequestBuilder,
    session: &SkypeSession,
    reg: &PollRegistration,
) -> RequestBuilder {
    let rb = authed(rb, session);
    match &reg.registration_token {
        Some(t) => rb.header("RegistrationToken", t),
        None => rb,
    }
}

pub(crate) async fn register(http: &Http, session: &SkypeSession) -> Result<PollRegistration> {
    let url = api_url(session, &["v1", "users", "ME", "endpoints"]);
    let resp = authed(http.client.post(url), session)
        .json(&json!({ "endpointFeatures": ENDPOINT_FEATURES }))
        .send()
        .await
        .map_err(net_err)?;
    let resp = check(resp).await?;
    let reg = PollRegistration {
        registration_token: resp
            .headers()
            .get("Set-RegistrationToken")
            .and_then(|v| v.to_str().ok())
            .and_then(parse_registration_token),
    };
    tracing::debug!(
        has_registration_token = reg.registration_token.is_some(),
        "endpoint vytvořen"
    );

    let url = api_url(
        session,
        &["v1", "users", "ME", "endpoints", "SELF", "subscriptions"],
    );
    let resp = with_registration(http.client.post(url), session, &reg)
        .json(&json!({
            "channelType": "httpLongPoll",
            "template": "raw",
            "interestedResources": INTERESTED_RESOURCES,
        }))
        .send()
        .await
        .map_err(net_err)?;
    check(resp).await?;
    Ok(reg)
}

pub(crate) async fn poll(
    http: &Http,
    session: &SkypeSession,
    reg: &PollRegistration,
) -> Result<Vec<Event>> {
    let url = api_url(
        session,
        &[
            "v1",
            "users",
            "ME",
            "endpoints",
            "SELF",
            "subscriptions",
            "0",
            "poll",
        ],
    );
    let resp = match with_registration(http.client.post(url), session, reg)
        .header(reqwest::header::CONTENT_LENGTH, 0)
        .timeout(POLL_TIMEOUT)
        .send()
        .await
    {
        Ok(r) => r,
        // Vypršení long-pollu není chyba, jen nic nepřišlo.
        Err(e) if e.is_timeout() => return Ok(vec![]),
        Err(e) => return Err(net_err(e)),
    };
    let resp = check(resp).await?;
    if resp.content_length() == Some(0) {
        return Ok(vec![]);
    }
    let r: PollResponse = json(resp).await?;
    Ok(r.event_messages
        .into_iter()
        .map(|e| e.into_domain())
        .collect())
}

/// `registrationToken=U2l...; expires=1700000000; endpointId={uuid}` → `registrationToken=U2l...`
fn parse_registration_token(header: &str) -> Option<String> {
    header
        .split(';')
        .map(str::trim)
        .find(|p| p.starts_with("registrationToken="))
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration as ChronoDuration, Utc};
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn parses_registration_header() {
        assert_eq!(
            parse_registration_token("registrationToken=abc; expires=1; endpointId={x}").as_deref(),
            Some("registrationToken=abc")
        );
        assert_eq!(parse_registration_token("garbage"), None);
    }

    #[tokio::test]
    async fn registers_then_polls_with_token() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/users/ME/endpoints"))
            .respond_with(ResponseTemplate::new(201).insert_header(
                "Set-RegistrationToken",
                "registrationToken=rt1; expires=9; endpointId={e}",
            ))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/users/ME/endpoints/SELF/subscriptions"))
            .and(header("RegistrationToken", "registrationToken=rt1"))
            .respond_with(ResponseTemplate::new(201))
            .expect(1)
            .mount(&server)
            .await;
        let poll_body = std::fs::read_to_string(format!(
            "{}/tests/fixtures/poll.json",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap();
        Mock::given(method("POST"))
            .and(path("/v1/users/ME/endpoints/SELF/subscriptions/0/poll"))
            .and(header("RegistrationToken", "registrationToken=rt1"))
            .and(header("Authentication", "skypetoken=st"))
            .respond_with(ResponseTemplate::new(200).set_body_string(poll_body))
            .mount(&server)
            .await;

        let http = Http::new().unwrap();
        let s = SkypeSession {
            token: "st".into(),
            expires_at: Utc::now() + ChronoDuration::hours(1),
            chat_service: server.uri().parse().unwrap(),
        };
        let reg = register(&http, &s).await.unwrap();
        let events = poll(&http, &s, &reg).await.unwrap();
        assert_eq!(events.len(), 4);
        assert!(matches!(events[0], Event::NewMessage(_)));
    }
}
