//! Chat služba (chatsvc): konverzace a zprávy.

pub(crate) mod wire;

use chrono::{TimeZone, Utc};
use reqwest::RequestBuilder;
use serde_json::{Value, json};
use tymy_core::{Conversation, ConversationId, Error, MessageId, MessagePage, Result, SentMessage};
use url::Url;

use crate::auth::SkypeSession;
use crate::http::{Http, SendLogged, check, json, net_err};

/// Počet zpráv na stránku historie.
const PAGE_SIZE: u32 = 50;
/// Stránka při indexaci historie na pozadí (server víc než 200 nedovolí).
pub const HISTORY_PAGE_SIZE: u32 = 200;
const VIEW: &str = "msnp24Equivalent|supportsMessageProperties";

/// Sestaví URL na chat službě; segmenty se správně procentově zakódují.
pub(crate) fn api_url(session: &SkypeSession, segments: &[&str]) -> Url {
    let mut url = session.chat_service.clone();
    url.path_segments_mut()
        .expect("chat_service je https URL")
        .clear()
        .extend(segments);
    url
}

pub(crate) fn authed(rb: RequestBuilder, session: &SkypeSession) -> RequestBuilder {
    rb.header("Authentication", format!("skypetoken={}", session.token))
}

pub async fn list_conversations(http: &Http, session: &SkypeSession) -> Result<Vec<Conversation>> {
    let url = api_url(session, &["v1", "users", "ME", "conversations"]);
    let resp = authed(http.client.get(url), session)
        .query(&[
            ("view", "msnp24Equivalent"),
            ("pageSize", "200"),
            ("targetType", "Passport|Skype|Lync|Thread|Agent|ShortCircuit|PSTN|Flxt|NotificationStream|ModernBots|secureThreads|InviteFree"),
        ])
        .send_logged()
        .await
        .map_err(net_err)?;
    let r: wire::ConversationsResponse = json(check(resp).await?).await?;
    let mut convs: Vec<_> = r
        .conversations
        .into_iter()
        .map(|c| c.into_domain())
        .collect();
    convs.sort_by_key(|c| std::cmp::Reverse(c.last_activity));
    Ok(convs)
}

/// `cursor` je `backwardLink` z předchozí stránky.
pub async fn list_messages(
    http: &Http,
    session: &SkypeSession,
    conversation: &ConversationId,
    cursor: Option<&str>,
) -> Result<MessagePage> {
    list_messages_sized(http, session, conversation, cursor, PAGE_SIZE).await
}

/// Jako [`list_messages`], ale s danou velikostí stránky (přepíše ji i v kurzoru).
pub async fn list_messages_sized(
    http: &Http,
    session: &SkypeSession,
    conversation: &ConversationId,
    cursor: Option<&str>,
    page_size: u32,
) -> Result<MessagePage> {
    let rb = match cursor {
        Some(link) => {
            let mut url = same_origin(session, link)?;
            let pairs: Vec<(String, String)> = url
                .query_pairs()
                .map(|(k, v)| {
                    let v = if k == "pageSize" {
                        page_size.to_string()
                    } else {
                        v.into_owned()
                    };
                    (k.into_owned(), v)
                })
                .collect();
            url.query_pairs_mut().clear().extend_pairs(pairs);
            http.client.get(url)
        }
        None => {
            let url = api_url(
                session,
                &[
                    "v1",
                    "users",
                    "ME",
                    "conversations",
                    conversation.as_str(),
                    "messages",
                ],
            );
            http.client.get(url).query(&[
                ("view", VIEW),
                ("pageSize", &page_size.to_string()),
                ("startTime", "1"),
            ])
        }
    };
    let resp = authed(rb, session).send_logged().await.map_err(net_err)?;
    let r: wire::MessagesResponse = json(check(resp).await?).await?;
    let messages: Vec<_> = r
        .messages
        .into_iter()
        .filter_map(|m| m.into_domain(Some(conversation.as_str())))
        .collect();
    // Prázdná stránka = konec historie, i kdyby server poslal další odkaz.
    let older = r.metadata.backward_link.filter(|_| !messages.is_empty());
    Ok(MessagePage { messages, older })
}

pub async fn send_html(
    http: &Http,
    session: &SkypeSession,
    conversation: &ConversationId,
    html: &str,
    display_name: Option<&str>,
) -> Result<SentMessage> {
    let url = api_url(
        session,
        &[
            "v1",
            "users",
            "ME",
            "conversations",
            conversation.as_str(),
            "messages",
        ],
    );
    let client_message_id = new_client_message_id();
    let body = json!({
        "content": html,
        "messagetype": "RichText/Html",
        "contenttype": "text",
        "clientmessageid": client_message_id,
        "imdisplayname": display_name.unwrap_or_default(),
    });
    let resp = authed(http.client.post(url), session)
        .json(&body)
        .send_logged()
        .await
        .map_err(net_err)?;
    let resp = check(resp).await?;
    // Odpověď bývá `{"OriginalArrivalTime": <ms>}`, ale nespoléháme na ni.
    let text = resp.text().await.map_err(net_err)?;
    let arrived_at = serde_json::from_str::<Value>(&text)
        .ok()
        .and_then(|v| v.get("OriginalArrivalTime").and_then(Value::as_i64))
        .and_then(|ms| Utc.timestamp_millis_opt(ms).single());
    Ok(SentMessage {
        client_message_id,
        arrived_at,
    })
}

/// URL jedné zprávy (`…/conversations/{id}/messages/{msg}`) s volitelnými dalšími segmenty.
fn message_url(
    session: &SkypeSession,
    conversation: &ConversationId,
    message: &MessageId,
    extra: &[&str],
) -> Url {
    let mut segments = vec![
        "v1",
        "users",
        "ME",
        "conversations",
        conversation.as_str(),
        "messages",
        message.as_str(),
    ];
    segments.extend_from_slice(extra);
    api_url(session, &segments)
}

/// Přidá (`on`) nebo odebere moji reakci na zprávu.
/// `PUT|DELETE …/messages/{id}/properties?name=emotions`, tělo `{"emotions":{"key":…,"value":ms}}`.
/// NEOVĚŘENO proti produkci: formát převzatý z web klienta Skype/Teams.
pub async fn set_reaction(
    http: &Http,
    session: &SkypeSession,
    conversation: &ConversationId,
    message: &MessageId,
    key: &str,
    on: bool,
) -> Result<()> {
    let mut url = message_url(session, conversation, message, &["properties"]);
    url.query_pairs_mut().append_pair("name", "emotions");
    let (req, body) = if on {
        (
            http.client.put(url),
            json!({"emotions": {"key": key, "value": Utc::now().timestamp_millis()}}),
        )
    } else {
        (http.client.delete(url), json!({"emotions": {"key": key}}))
    };
    let resp = authed(req, session)
        .json(&body)
        .send_logged()
        .await
        .map_err(net_err)?;
    check(resp).await.map(|_| ())
}

/// Přepíše obsah vlastní zprávy (`PUT …/messages/{id}`).
/// NEOVĚŘENO proti produkci.
pub async fn edit_html(
    http: &Http,
    session: &SkypeSession,
    conversation: &ConversationId,
    message: &MessageId,
    html: &str,
    display_name: Option<&str>,
) -> Result<()> {
    let url = message_url(session, conversation, message, &[]);
    let body = json!({
        "id": message.as_str(),
        "type": "Message",
        "conversationid": conversation.as_str(),
        "content": html,
        "messagetype": "RichText/Html",
        "contenttype": "text",
        "imdisplayname": display_name.unwrap_or_default(),
    });
    let resp = authed(http.client.put(url), session)
        .json(&body)
        .send_logged()
        .await
        .map_err(net_err)?;
    check(resp).await.map(|_| ())
}

/// Smaže vlastní zprávu (`DELETE …/messages/{id}?behavior=softDelete`; ostatní uvidí
/// „Zpráva byla smazána“). NEOVĚŘENO proti produkci.
pub async fn delete_message(
    http: &Http,
    session: &SkypeSession,
    conversation: &ConversationId,
    message: &MessageId,
) -> Result<()> {
    let mut url = message_url(session, conversation, message, &[]);
    url.query_pairs_mut().append_pair("behavior", "softDelete");
    let resp = authed(http.client.delete(url), session)
        .send_logged()
        .await
        .map_err(net_err)?;
    check(resp).await.map(|_| ())
}

/// Založí vlákno chatu (`POST /v1/threads`). Pro dvě osoby žádá „unikátní“ 1:1 chat.
/// NEOVĚŘENO proti produkci: formát převzatý z protokolu Skype.
pub async fn create_thread(
    http: &Http,
    session: &SkypeSession,
    me: &str,
    members: &[&str],
) -> Result<ConversationId> {
    let mut list = vec![json!({"id": me, "role": "Admin"})];
    list.extend(members.iter().map(|m| json!({"id": m, "role": "Admin"})));
    let one_on_one = members.len() == 1;
    let body = json!({
        "members": list,
        "properties": {
            "threadType": "chat",
            "fixedRoster": one_on_one.to_string(),
            "uniquerosterthread": one_on_one.to_string(),
        }
    });
    let url = api_url(session, &["v1", "threads"]);
    let resp = authed(http.client.post(url), session)
        .json(&body)
        .send_logged()
        .await
        .map_err(net_err)?;
    let resp = check(resp).await?;
    let location = resp
        .headers()
        .get(reqwest::header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let text = resp.text().await.map_err(net_err)?;
    tracing::debug!(?location, body = %crate::redact(&text), "vlákno založeno");
    let from_body = || {
        let v: Value = serde_json::from_str(&text).ok()?;
        v.get("threadResource")
            .and_then(|t| t.get("id"))
            .or_else(|| v.get("id"))
            .and_then(Value::as_str)
            .map(str::to_owned)
    };
    location
        .as_deref()
        .and_then(|l| l.rsplit_once("/threads/").map(|(_, id)| id.to_owned()))
        .or_else(from_body)
        .filter(|id| id.starts_with("19:"))
        .map(ConversationId)
        .ok_or_else(|| Error::Protocol("server nevrátil id založeného chatu".into()))
}

/// Ladění: GET na libovolnou cestu chat služby (`/v1/...?...`), vrátí surový JSON.
pub async fn raw_get(http: &Http, session: &SkypeSession, path_and_query: &str) -> Result<Value> {
    if !path_and_query.starts_with('/') {
        return Err(Error::Other("cesta musí začínat '/'".into()));
    }
    let url = session
        .chat_service
        .join(path_and_query)
        .map_err(|e| Error::Other(format!("cesta {path_and_query}: {e}")))?;
    let url = same_origin(session, url.as_str())?;
    let resp = authed(http.client.get(url), session)
        .send_logged()
        .await
        .map_err(net_err)?;
    json(check(resp).await?).await
}

/// Teams používá 19místná náhodná čísla.
fn new_client_message_id() -> String {
    let n: u64 = rand::random_range(1_000_000_000_000_000_000..10_000_000_000_000_000_000);
    n.to_string()
}

/// Odkazy ze serveru (stránkování) následujeme jen na stejný host, jinak by
/// podvržený odkaz mohl vylákat skypetoken.
fn same_origin(session: &SkypeSession, link: &str) -> Result<Url> {
    let url = Url::parse(link).map_err(|e| Error::Protocol(format!("odkaz {link}: {e}")))?;
    if url.origin() != session.chat_service.origin() {
        return Err(Error::Protocol(format!(
            "odkaz míří mimo chat službu: {}",
            url.host_str().unwrap_or("?")
        )));
    }
    Ok(url)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
    use wiremock::matchers::{header, method, path, query_param};
    use wiremock::{Mock, MockServer, Request, ResponseTemplate};

    fn session(base: &str) -> SkypeSession {
        SkypeSession {
            token: "st".into(),
            expires_at: Utc::now() + Duration::hours(1),
            chat_service: Url::parse(base).unwrap(),
            bearer: String::new(),
            middle_tier: None,
        }
    }

    #[test]
    fn url_encodes_conversation_id() {
        let s = session("https://emea.ng.msg.teams.microsoft.com");
        let u = api_url(
            &s,
            &[
                "v1",
                "users",
                "ME",
                "conversations",
                "19:a/b@thread.v2",
                "messages",
            ],
        );
        assert_eq!(
            u.as_str(),
            "https://emea.ng.msg.teams.microsoft.com/v1/users/ME/conversations/19:a%2Fb@thread.v2/messages"
        );
    }

    #[test]
    fn rejects_foreign_cursor() {
        let s = session("https://emea.ng.msg.teams.microsoft.com");
        assert!(same_origin(&s, "https://evil.example/v1/x").is_err());
        assert!(same_origin(&s, "https://emea.ng.msg.teams.microsoft.com/v1/x?a=1").is_ok());
    }

    #[test]
    fn client_message_id_has_19_digits() {
        for _ in 0..100 {
            assert_eq!(new_client_message_id().len(), 19);
        }
    }

    #[tokio::test]
    async fn reacts_edits_and_deletes() {
        let server = MockServer::start().await;
        let msg = "/v1/users/ME/conversations/19:abc@thread.v2/messages/1700000000000";
        Mock::given(method("PUT"))
            .and(path(format!("{msg}/properties")))
            .and(query_param("name", "emotions"))
            .and(header("Authentication", "skypetoken=st"))
            .respond_with(|req: &Request| {
                let body: Value = serde_json::from_slice(&req.body).unwrap();
                assert_eq!(body["emotions"]["key"], "like");
                assert!(body["emotions"]["value"].as_i64().unwrap() > 1_700_000_000_000);
                ResponseTemplate::new(200)
            })
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .and(path(format!("{msg}/properties")))
            .and(query_param("name", "emotions"))
            .respond_with(|req: &Request| {
                let body: Value = serde_json::from_slice(&req.body).unwrap();
                assert_eq!(body, json!({"emotions": {"key": "heart"}}));
                ResponseTemplate::new(200)
            })
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("PUT"))
            .and(path(msg))
            .respond_with(|req: &Request| {
                let body: Value = serde_json::from_slice(&req.body).unwrap();
                assert_eq!(body["content"], "<p>Opraveno</p>");
                assert_eq!(body["id"], "1700000000000");
                ResponseTemplate::new(200)
            })
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .and(path(msg))
            .and(query_param("behavior", "softDelete"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;
        let (http, s) = (Http::new().unwrap(), session(&server.uri()));
        let (conv, id) = (
            ConversationId::from("19:abc@thread.v2"),
            MessageId::from("1700000000000"),
        );
        set_reaction(&http, &s, &conv, &id, "like", true)
            .await
            .unwrap();
        set_reaction(&http, &s, &conv, &id, "heart", false)
            .await
            .unwrap();
        edit_html(&http, &s, &conv, &id, "<p>Opraveno</p>", Some("Jan"))
            .await
            .unwrap();
        delete_message(&http, &s, &conv, &id).await.unwrap();
    }

    #[tokio::test]
    async fn sends_message_with_skypetoken() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/users/ME/conversations/19:abc@thread.v2/messages"))
            .and(header("Authentication", "skypetoken=st"))
            .respond_with(|req: &Request| {
                let body: Value = serde_json::from_slice(&req.body).unwrap();
                assert_eq!(body["content"], "<p>Ahoj</p>");
                assert_eq!(body["messagetype"], "RichText/Html");
                assert_eq!(body["imdisplayname"], "Jan");
                ResponseTemplate::new(201)
                    .set_body_json(json!({"OriginalArrivalTime": 1700000000000i64}))
            })
            .expect(1)
            .mount(&server)
            .await;
        let s = session(&server.uri());
        let sent = send_html(
            &Http::new().unwrap(),
            &s,
            &"19:abc@thread.v2".into(),
            "<p>Ahoj</p>",
            Some("Jan"),
        )
        .await
        .unwrap();
        assert_eq!(sent.arrived_at.unwrap().timestamp_millis(), 1700000000000);
    }

    #[tokio::test]
    async fn pages_through_history() {
        let server = MockServer::start().await;
        let fixture = std::fs::read_to_string(format!(
            "{}/tests/fixtures/messages.json",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap()
        // backwardLink ve fixtuře přesměrujeme na mock server
        .replace("https://emea.ng.msg.teams.microsoft.com", &server.uri());
        Mock::given(method("GET"))
            .and(path("/v1/users/ME/conversations/19:c@thread.v2/messages"))
            .and(query_param("startTime", "1"))
            .and(query_param("pageSize", "50"))
            .respond_with(ResponseTemplate::new(200).set_body_string(fixture))
            .mount(&server)
            .await;
        let s = session(&server.uri());
        let page = list_messages(&Http::new().unwrap(), &s, &"19:c@thread.v2".into(), None)
            .await
            .unwrap();
        assert_eq!(page.messages.len(), 3);
        assert!(page.older.as_deref().unwrap().starts_with(&server.uri()));
    }
}
