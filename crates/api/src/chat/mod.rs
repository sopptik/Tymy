//! Chat služba (chatsvc): konverzace a zprávy.

pub(crate) mod wire;

use chrono::{TimeZone, Utc};
use reqwest::RequestBuilder;
use serde_json::{Value, json};
use tymy_core::{
    Conversation, ConversationId, ConversationPage, Error, MessageId, MessagePage, Result,
    SentMessage, UserId,
};
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

/// Nejvýš kolik stránek seznamu konverzací (po 200) načíst najednou („celý seznam“).
pub const MAX_CONVERSATION_PAGES: usize = 10;

/// Seznam konverzací od nejnovější aktivity: `max_pages` stránek po 200 přes
/// `_metadata.backwardLink` (ověřeno 2026-11-04), od začátku (`cursor = None`) nebo od
/// kurzoru z předchozího volání. Vrácený `older` = kurzor na další stránku, `None` = konec.
pub async fn list_conversations(
    http: &Http,
    session: &SkypeSession,
    cursor: Option<&str>,
    max_pages: usize,
) -> Result<ConversationPage> {
    // ladění stránkování: `TYMY_CONVERSATION_PAGE_SIZE=10`
    let page_size = std::env::var("TYMY_CONVERSATION_PAGE_SIZE")
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(200)
        .to_string();
    let mut rb = match cursor {
        Some(link) => http.client.get(same_origin(session, link)?),
        None => http
            .client
            .get(api_url(session, &["v1", "users", "ME", "conversations"]))
            .query(&[
                ("view", "msnp24Equivalent"),
                ("pageSize", page_size.as_str()),
                ("targetType", "Passport|Skype|Lync|Thread|Agent|ShortCircuit|PSTN|Flxt|NotificationStream|ModernBots|secureThreads|InviteFree"),
            ]),
    };
    let mut convs: Vec<Conversation> = vec![];
    let mut seen = std::collections::HashSet::new();
    let mut deleted = std::collections::HashSet::new();
    let mut older = None;
    for page in 1..=max_pages.max(1) {
        let resp = authed(rb, session).send_logged().await.map_err(net_err)?;
        let r: wire::ConversationsResponse = json(check(resp).await?).await?;
        let empty = r.conversations.is_empty();
        // kanály smazaného týmu příznak smazání nemají, jen odkaz na tým (`spaceId`)
        deleted.extend(
            r.conversations
                .iter()
                .filter(|c| c.deleted())
                .map(|c| c.id.clone()),
        );
        convs.extend(
            r.conversations
                .into_iter()
                // Opuštěné týmy a smazaná vlákna: oficiální klient je nezobrazuje, server
                // k nim odpírá přístup (403) nebo je už nemá (404).
                .filter(|c| !c.left_team() && !c.deleted())
                .filter(|c| c.team_id().is_none_or(|t| !deleted.contains(t)))
                .map(|c| c.into_domain())
                .filter(|c| seen.insert(c.id.clone())),
        );
        // prázdná stránka = konec seznamu, i kdyby server poslal další odkaz
        let Some(next) = r.metadata.backward_link.filter(|_| !empty) else {
            break;
        };
        if page == max_pages.max(1) {
            older = Some(next);
            break;
        }
        rb = http.client.get(same_origin(session, &next)?);
    }
    convs.sort_by_key(|c| std::cmp::Reverse(c.last_activity));
    Ok(ConversationPage {
        conversations: convs,
        older,
    })
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
    ams_references: &[String],
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
    let mut body = json!({
        "content": html,
        "messagetype": "RichText/Html",
        "contenttype": "text",
        "clientmessageid": client_message_id,
        "imdisplayname": display_name.unwrap_or_default(),
    });
    // vložené obrázky: id objektů v media službě (jako web klient)
    if !ams_references.is_empty() {
        body["amsreferences"] = json!(ams_references);
    }
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

/// Formát `consumptionhorizon`: `<id přečtené zprávy>;<čas přečtení v ms>;<clientmessageid>`
/// (shodně s hodnotou, kterou vrací seznam konverzací).
pub fn consumption_horizon(message: &MessageId, client_id: Option<&str>, now_ms: i64) -> String {
    format!(
        "{};{now_ms};{}",
        message.as_str(),
        client_id
            .filter(|c| !c.is_empty())
            .unwrap_or(message.as_str())
    )
}

/// Nahlásí serveru, kam až uživatel konverzaci přečetl (ostatní klienti pak nepřečtené neukazují).
/// `PUT /v1/users/ME/conversations/{id}/properties?name=consumptionhorizon`,
/// tělo `{"consumptionhorizon": "<id>;<čas>;<clientmessageid>"}`.
/// NEOVĚŘENO proti produkci: formát převzatý z protokolu Skype a hodnot ze seznamu konverzací.
pub async fn set_consumption_horizon(
    http: &Http,
    session: &SkypeSession,
    conversation: &ConversationId,
    horizon: &str,
) -> Result<()> {
    let mut url = api_url(
        session,
        &[
            "v1",
            "users",
            "ME",
            "conversations",
            conversation.as_str(),
            "properties",
        ],
    );
    url.query_pairs_mut()
        .append_pair("name", "consumptionhorizon");
    let resp = authed(http.client.put(url), session)
        .json(&json!({ "consumptionhorizon": horizon }))
        .send_logged()
        .await
        .map_err(net_err)?;
    check(resp).await.map(|_| ())
}

/// Založí vlákno chatu (`POST /v1/threads`). Pro dvě osoby žádá „unikátní“ 1:1 chat,
/// pro víc skupinový chat (s názvem `topic`, je-li zadaný).
/// NEOVĚŘENO proti produkci: formát převzatý z protokolu Skype.
pub async fn create_thread(
    http: &Http,
    session: &SkypeSession,
    me: &str,
    members: &[&str],
    topic: Option<&str>,
) -> Result<ConversationId> {
    let mut list = vec![json!({"id": me, "role": "Admin"})];
    list.extend(members.iter().map(|m| json!({"id": m, "role": "Admin"})));
    let one_on_one = members.len() == 1;
    let mut properties = json!({
        "threadType": "chat",
        "fixedRoster": one_on_one.to_string(),
        "uniquerosterthread": one_on_one.to_string(),
    });
    if let Some(topic) = topic.filter(|t| !t.trim().is_empty()) {
        properties["topic"] = json!(topic.trim());
    }
    let body = json!({
        "members": list,
        "properties": properties,
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

/// Členové vlákna (skupinového chatu): `GET /v1/threads/{id}?view=msnp24Equivalent`
/// → `{members: [{id: "8:orgid:…", role}, …]}`. NEOVĚŘENO proti produkci (protokol Skype).
pub async fn thread_members(
    http: &Http,
    session: &SkypeSession,
    conversation: &ConversationId,
) -> Result<Vec<UserId>> {
    let mut url = api_url(session, &["v1", "threads", conversation.as_str()]);
    url.query_pairs_mut()
        .append_pair("view", "msnp24Equivalent");
    let resp = authed(http.client.get(url), session)
        .send_logged()
        .await
        .map_err(net_err)?;
    let v: Value = json(check(resp).await?).await?;
    Ok(v.get("members")
        .and_then(Value::as_array)
        .map(|m| {
            m.iter()
                .filter_map(|x| x.get("id").and_then(Value::as_str))
                .filter(|id| id.starts_with("8:"))
                .map(|id| UserId(id.to_owned()))
                .collect()
        })
        .unwrap_or_default())
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
    async fn creates_group_chat_with_topic() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/threads"))
            .and(header("Authentication", "skypetoken=st"))
            .respond_with(|req: &Request| {
                let body: Value = serde_json::from_slice(&req.body).unwrap();
                assert_eq!(body["members"].as_array().unwrap().len(), 3);
                assert_eq!(body["members"][0]["id"], "8:orgid:me");
                assert_eq!(body["properties"]["threadType"], "chat");
                assert_eq!(body["properties"]["fixedRoster"], "false");
                assert_eq!(body["properties"]["topic"], "Projekt");
                ResponseTemplate::new(201).insert_header(
                    "Location",
                    "https://emea.ng.msg.teams.microsoft.com/v1/threads/19:new@thread.v2",
                )
            })
            .expect(1)
            .mount(&server)
            .await;
        let id = create_thread(
            &Http::new().unwrap(),
            &session(&server.uri()),
            "8:orgid:me",
            &["8:orgid:a", "8:orgid:b"],
            Some(" Projekt "),
        )
        .await
        .unwrap();
        assert_eq!(id.as_str(), "19:new@thread.v2");
    }

    #[tokio::test]
    async fn lists_thread_members() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/threads/19:g@thread.v2"))
            .and(query_param("view", "msnp24Equivalent"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "19:g@thread.v2",
                "members": [
                    {"id": "8:orgid:a", "role": "Admin"},
                    {"id": "28:bot", "role": "User"},
                    {"id": "8:orgid:b", "role": "User"}
                ]
            })))
            .mount(&server)
            .await;
        let m = thread_members(
            &Http::new().unwrap(),
            &session(&server.uri()),
            &"19:g@thread.v2".into(),
        )
        .await
        .unwrap();
        let ids: Vec<_> = m.iter().map(|u| u.as_str()).collect();
        assert_eq!(ids, ["8:orgid:a", "8:orgid:b"], "boti se vynechají");
    }

    #[tokio::test]
    async fn skips_deleted_teams_and_their_channels() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/users/ME/conversations"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "conversations": [
                    {"id": "19:gone@thread.tacv2", "threadProperties": {"threadType": "space", "isdeleted": true}},
                    {"id": "19:ch@thread.tacv2", "threadProperties": {"threadType": "topic", "spaceId": "19:gone@thread.tacv2"}},
                    {"id": "19:team@thread.tacv2", "threadProperties": {"threadType": "space"}},
                    {"id": "19:ok@thread.tacv2", "threadProperties": {"threadType": "topic", "spaceId": "19:team@thread.tacv2"}}
                ],
                "_metadata": {"backwardLink": null}
            })))
            .mount(&server)
            .await;
        let (http, s) = (Http::new().unwrap(), session(&server.uri()));
        let page = list_conversations(&http, &s, None, 1).await.unwrap();
        let mut ids: Vec<_> = page.conversations.iter().map(|c| c.id.as_str()).collect();
        ids.sort();
        assert_eq!(ids, ["19:ok@thread.tacv2", "19:team@thread.tacv2"]);
    }

    #[tokio::test]
    async fn follows_conversation_pages() {
        let server = MockServer::start().await;
        let next = format!(
            "{}/v1/users/8:orgid:me/conversations?view=msnp24Equivalent&syncState=abc&pageSize=200",
            server.uri()
        );
        Mock::given(method("GET"))
            .and(path("/v1/users/ME/conversations"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "conversations": [{"id": "19:a@thread.v2", "threadProperties": {"threadType": "chat"}}],
                "_metadata": {"backwardLink": next}
            })))
            .expect(2)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/users/8:orgid:me/conversations"))
            .and(query_param("syncState", "abc"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "conversations": [
                    {"id": "19:a@thread.v2", "threadProperties": {"threadType": "chat"}},
                    {"id": "19:b@thread.v2", "threadProperties": {"threadType": "chat"}}
                ],
                "_metadata": {"backwardLink": null}
            })))
            .expect(2)
            .mount(&server)
            .await;
        let (http, s) = (Http::new().unwrap(), session(&server.uri()));
        let all = list_conversations(&http, &s, None, MAX_CONVERSATION_PAGES)
            .await
            .unwrap();
        let ids: Vec<_> = all.conversations.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids.len(), 2, "duplicita mezi stránkami se vynechá: {ids:?}");
        assert_eq!(all.older, None, "seznam je celý");
        let first = list_conversations(&http, &s, None, 1).await.unwrap();
        assert_eq!(first.conversations.len(), 1, "jen první stránka");
        let cursor = first.older.expect("kurzor na další stránku");
        let rest = list_conversations(&http, &s, Some(&cursor), 1)
            .await
            .unwrap();
        assert_eq!(rest.conversations.len(), 2);
        assert_eq!(rest.older, None);
    }

    #[tokio::test]
    async fn reports_read_horizon() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path(
                "/v1/users/ME/conversations/19:abc@thread.v2/properties",
            ))
            .and(query_param("name", "consumptionhorizon"))
            .and(header("Authentication", "skypetoken=st"))
            .respond_with(|req: &Request| {
                let body: Value = serde_json::from_slice(&req.body).unwrap();
                assert_eq!(
                    body,
                    json!({"consumptionhorizon": "1700000000000;1700000005000;42"})
                );
                ResponseTemplate::new(200)
            })
            .expect(1)
            .mount(&server)
            .await;
        let id = MessageId::from("1700000000000");
        let h = consumption_horizon(&id, Some("42"), 1_700_000_005_000);
        assert_eq!(
            consumption_horizon(&id, None, 5),
            "1700000000000;5;1700000000000",
            "bez clientmessageid se použije id"
        );
        set_consumption_horizon(
            &Http::new().unwrap(),
            &session(&server.uri()),
            &"19:abc@thread.v2".into(),
            &h,
        )
        .await
        .unwrap();
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
                assert_eq!(body["amsreferences"], json!(["0-weu-d1-x"]));
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
            &["0-weu-d1-x".to_owned()],
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
