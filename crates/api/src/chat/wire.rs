//! Drátové (JSON) typy chat služby a jejich převod na doménový model.
//!
//! Všechna pole jsou volitelná: server je vynechává podle typu konverzace i verze API
//! a jedno chybějící pole nesmí shodit načtení celého seznamu.

use chrono::{DateTime, TimeZone, Utc};
use serde::Deserialize;
use serde_json::Value;
use tymy_core::{
    Attachment, Conversation, ConversationId, ConversationKind, Event, Message, MessageKind,
    Reaction, TeamRef, UserId,
};

#[derive(Debug, Default, Deserialize)]
pub(crate) struct ConversationsResponse {
    #[serde(default)]
    pub conversations: Vec<WireConversation>,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct MessagesResponse {
    #[serde(default)]
    pub messages: Vec<WireMessage>,
    #[serde(rename = "_metadata", default)]
    pub metadata: Metadata,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Metadata {
    pub backward_link: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WireConversation {
    pub id: String,
    #[serde(default)]
    pub thread_properties: Option<ThreadProperties>,
    #[serde(default)]
    pub last_message: Option<WireMessage>,
    #[serde(default)]
    pub properties: Option<ConversationProperties>,
    #[serde(default)]
    pub last_updated_message_arrival_time: Option<i64>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ThreadProperties {
    pub topic: Option<String>,
    pub thread_type: Option<String>,
    pub product_thread_type: Option<String>,
    /// U kanálu: id kořenového vlákna týmu.
    pub space_id: Option<String>,
    /// U kořenového vlákna týmu: název týmu.
    pub space_thread_topic: Option<String>,
    /// Poslední připojení / odchod uživatele z vlákna (ms, jako text i číslo).
    #[serde(default, rename = "lastjoinat")]
    pub last_join_at: Option<Value>,
    #[serde(default, rename = "lastleaveat")]
    pub last_leave_at: Option<Value>,
}

/// Čas v ms z JSON hodnoty (text nebo číslo).
fn ms_value(v: Option<&Value>) -> i64 {
    match v {
        Some(Value::String(s)) => s.parse().unwrap_or(0),
        Some(Value::Number(n)) => n.as_i64().unwrap_or(0),
        _ => 0,
    }
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct ConversationProperties {
    /// Formát `<id poslední přečtené>;<čas>;<clientmessageid>`.
    pub consumptionhorizon: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WireMessage {
    pub id: Option<String>,
    pub clientmessageid: Option<String>,
    pub content: Option<String>,
    pub messagetype: Option<String>,
    pub from: Option<String>,
    pub imdisplayname: Option<String>,
    pub composetime: Option<String>,
    pub originalarrivaltime: Option<String>,
    pub conversation_link: Option<String>,
    /// Některé odpovědi nesou id konverzace přímo.
    pub conversationid: Option<String>,
    #[serde(default)]
    pub properties: Option<Value>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct PollResponse {
    #[serde(rename = "eventMessages", default)]
    pub event_messages: Vec<EventMessage>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct EventMessage {
    #[serde(default)]
    pub resource_type: String,
    #[serde(default)]
    pub resource_link: Option<String>,
    #[serde(default)]
    pub resource: Value,
}

impl WireConversation {
    /// Kanál nebo tým, který uživatel opustil (odchod je pozdější než poslední připojení).
    /// Seznam konverzací je dál vrací, oficiální klient je ale nezobrazuje (týmy bere
    /// z jiné služby) a server k nim často odepře přístup (403).
    pub fn left_team(&self) -> bool {
        let Some(p) = &self.thread_properties else {
            return false;
        };
        let channel = conversation_kind(&self.id, Some(p)) == ConversationKind::Channel;
        let left = ms_value(p.last_leave_at.as_ref());
        channel && left > 0 && left > ms_value(p.last_join_at.as_ref())
    }

    pub fn into_domain(self) -> Conversation {
        let kind = conversation_kind(&self.id, self.thread_properties.as_ref());
        let last_message = self
            .last_message
            .and_then(|m| m.into_domain(Some(&self.id)));
        let last_activity = self
            .last_updated_message_arrival_time
            .and_then(millis)
            .or_else(|| last_message.as_ref().and_then(|m| m.arrived_at));
        let read_up_to = self
            .properties
            .and_then(|p| p.consumptionhorizon)
            .and_then(|h| h.split(';').next().map(str::to_owned))
            .filter(|id| !id.is_empty() && id != "0")
            .map(Into::into);
        let team = self.thread_properties.as_ref().and_then(|t| {
            let name = t.space_thread_topic.clone().filter(|n| !n.is_empty());
            match (t.thread_type.as_deref(), &t.space_id, name) {
                // kořen týmu = obecný kanál
                (Some("space"), _, name) => Some(TeamRef {
                    id: ConversationId(self.id.clone()),
                    name,
                }),
                (_, Some(space), name) if !space.is_empty() => Some(TeamRef {
                    id: ConversationId(space.clone()),
                    name,
                }),
                _ => None,
            }
        });
        Conversation {
            id: ConversationId(self.id),
            kind,
            topic: self
                .thread_properties
                .and_then(|t| t.topic)
                .filter(|t| !t.is_empty()),
            team,
            last_message,
            last_activity,
            read_up_to,
        }
    }
}

fn conversation_kind(id: &str, props: Option<&ThreadProperties>) -> ConversationKind {
    if id.starts_with("48:") || id.starts_with("28:") {
        return ConversationKind::System;
    }
    if id.ends_with("@unq.gbl.spaces") || id.starts_with("8:") {
        return ConversationKind::OneOnOne;
    }
    // Kanály týmů; skupinové chaty mají `@thread.v2`.
    if id.ends_with("@thread.tacv2") || id.ends_with("@thread.skype") {
        return ConversationKind::Channel;
    }
    let thread_type = props
        .and_then(|p| p.thread_type.as_deref())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let product = props
        .and_then(|p| p.product_thread_type.as_deref())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if id.contains("meeting_") || thread_type == "meeting" || product.contains("meeting") {
        ConversationKind::Meeting
    } else if thread_type == "space" || thread_type == "topic" || product.contains("channel") {
        ConversationKind::Channel
    } else if product.contains("oneonone") {
        ConversationKind::OneOnOne
    } else if thread_type == "chat" || product == "chat" {
        ConversationKind::Group
    } else {
        ConversationKind::Other(if product.is_empty() {
            thread_type
        } else {
            product
        })
    }
}

impl WireMessage {
    /// `conversation_hint` se použije, když zpráva sama neuvádí, kam patří.
    /// Vrací `None` pro prázdné objekty (např. `lastMessage: {}`).
    pub fn into_domain(self, conversation_hint: Option<&str>) -> Option<Message> {
        let id = self.id.filter(|s| !s.is_empty())?;
        let conversation_id = self
            .conversationid
            .or_else(|| {
                self.conversation_link
                    .as_deref()
                    .and_then(|l| after_last(l, "/conversations/"))
            })
            .or_else(|| conversation_hint.map(str::to_owned))?;
        let messagetype = self.messagetype.unwrap_or_default();
        let props = self.properties.unwrap_or(Value::Null);
        let deleted = prop_time(&props, "deletetime").is_some()
            || props.get("deletetime").is_some_and(|v| !v.is_null());
        Some(Message {
            id: id.into(),
            conversation_id: conversation_id.into(),
            client_message_id: self.clientmessageid,
            sender: self
                .from
                .as_deref()
                .and_then(|f| after_last(f, "/contacts/").or_else(|| Some(f.to_owned())))
                .filter(|s| !s.is_empty())
                .map(UserId),
            sender_name: self.imdisplayname.filter(|n| !n.is_empty()),
            kind: message_kind(&messagetype),
            content: self.content.unwrap_or_default(),
            composed_at: self.composetime.as_deref().and_then(rfc3339),
            arrived_at: self.originalarrivaltime.as_deref().and_then(rfc3339),
            edited_at: prop_time(&props, "edittime"),
            deleted,
            attachments: attachments(&props),
            reactions: reactions(&props),
        })
    }
}

impl EventMessage {
    pub fn into_domain(self) -> Event {
        let resource_type = self.resource_type;
        let raw = self.resource;
        match resource_type.as_str() {
            "NewMessage" | "MessageUpdate" => {
                let messagetype = raw.get("messagetype").and_then(Value::as_str).unwrap_or("");
                let hint = self
                    .resource_link
                    .as_deref()
                    .and_then(|l| after_last(l, "/conversations/"))
                    .and_then(|s| s.split('/').next().map(str::to_owned));
                if messagetype.starts_with("Control/") {
                    let wire: WireMessage = serde_json::from_value(raw.clone()).unwrap_or_default();
                    if let Some(conversation_id) = wire
                        .conversation_link
                        .as_deref()
                        .and_then(|l| after_last(l, "/conversations/"))
                        .or(hint)
                        .filter(|_| messagetype.contains("Typing"))
                    {
                        return Event::Typing {
                            conversation_id: conversation_id.into(),
                            user: wire
                                .from
                                .as_deref()
                                .and_then(|f| after_last(f, "/contacts/"))
                                .map(UserId),
                        };
                    }
                    return Event::Unknown { resource_type, raw };
                }
                match serde_json::from_value::<WireMessage>(raw.clone())
                    .ok()
                    .and_then(|m| m.into_domain(hint.as_deref()))
                {
                    Some(m) if resource_type == "NewMessage" => Event::NewMessage(m),
                    Some(m) => Event::MessageUpdated(m),
                    None => Event::Unknown { resource_type, raw },
                }
            }
            "ConversationUpdate" | "ThreadUpdate" => {
                match raw
                    .get("id")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .or_else(|| {
                        self.resource_link.as_deref().and_then(|l| {
                            after_last(l, "/conversations/").or_else(|| after_last(l, "/threads/"))
                        })
                    }) {
                    Some(id) => Event::ConversationUpdated(id.into()),
                    None => Event::Unknown { resource_type, raw },
                }
            }
            _ => Event::Unknown { resource_type, raw },
        }
    }
}

fn message_kind(t: &str) -> MessageKind {
    match t {
        "Text" => MessageKind::Text,
        "RichText" | "RichText/Html" => MessageKind::RichText,
        t if t.starts_with("Event/")
            || t.starts_with("ThreadActivity/")
            || t.starts_with("RichText/Media_") =>
        {
            MessageKind::Event(t.to_owned())
        }
        t => MessageKind::Unknown(t.to_owned()),
    }
}

/// Přílohy jsou v `properties.files`, typicky jako JSON *řetězec* obsahující pole.
fn attachments(props: &Value) -> Vec<Attachment> {
    let files = match props.get("files") {
        Some(Value::String(s)) => serde_json::from_str::<Value>(s).unwrap_or(Value::Null),
        Some(v) => v.clone(),
        None => return vec![],
    };
    let Value::Array(items) = files else {
        return vec![];
    };
    items
        .iter()
        .filter_map(|f| {
            let s = |k: &str| f.get(k).and_then(Value::as_str).filter(|s| !s.is_empty());
            let info = |k: &str| {
                f.get("fileInfo")
                    .and_then(|i| i.get(k))
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
            };
            let name = s("fileName")
                .or_else(|| s("title"))
                .or_else(|| info("fileName"))?;
            Some(Attachment {
                name: name.to_owned(),
                url: info("shareUrl")
                    .or_else(|| s("objectUrl"))
                    .or_else(|| info("fileUrl"))
                    .map(str::to_owned),
                size: f.get("fileSize").and_then(Value::as_u64),
                file_type: s("fileType").map(str::to_owned),
            })
        })
        .collect()
}

/// Reakce jsou v `properties.emotions`, jako pole nebo JSON řetězec s polem.
fn reactions(props: &Value) -> Vec<Reaction> {
    let value = match props.get("emotions") {
        Some(Value::String(s)) => serde_json::from_str::<Value>(s).unwrap_or(Value::Null),
        Some(v) => v.clone(),
        None => return vec![],
    };
    let Value::Array(items) = value else {
        return vec![];
    };
    items
        .iter()
        .filter_map(|e| {
            let key = e.get("key")?.as_str()?.to_owned();
            let users: Vec<UserId> = e
                .get("users")?
                .as_array()?
                .iter()
                .filter_map(|u| u.get("mri")?.as_str().map(|m| UserId(m.to_owned())))
                .collect();
            (!users.is_empty()).then_some(Reaction { key, users })
        })
        .collect()
}

fn prop_time(props: &Value, key: &str) -> Option<DateTime<Utc>> {
    let v = props.get(key)?;
    let ms = v.as_i64().or_else(|| v.as_str()?.parse().ok())?;
    millis(ms)
}

fn millis(ms: i64) -> Option<DateTime<Utc>> {
    if ms <= 0 {
        return None;
    }
    Utc.timestamp_millis_opt(ms).single()
}

fn rfc3339(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|d| d.with_timezone(&Utc))
}

fn after_last(s: &str, marker: &str) -> Option<String> {
    s.rfind(marker)
        .map(|i| s[i + marker.len()..].to_owned())
        .filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> String {
        std::fs::read_to_string(format!(
            "{}/tests/fixtures/{name}",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap()
    }

    #[test]
    fn parses_conversation_list() {
        let r: ConversationsResponse =
            serde_json::from_str(&fixture("conversations.json")).unwrap();
        let convs: Vec<_> = r
            .conversations
            .into_iter()
            .map(|c| c.into_domain())
            .collect();
        assert_eq!(convs.len(), 4);

        let one = &convs[0];
        assert_eq!(one.kind, ConversationKind::OneOnOne);
        let last = one.last_message.as_ref().unwrap();
        assert_eq!(
            last.sender.as_ref().unwrap().as_str(),
            "8:orgid:11111111-1111-1111-1111-111111111111"
        );
        assert_eq!(last.sender_name.as_deref(), Some("Alice Nováková"));
        assert_eq!(last.conversation_id, one.id);
        assert_eq!(one.read_up_to.as_ref().unwrap().as_str(), "1700000000000");
        assert!(one.has_unread());

        let group = &convs[1];
        assert_eq!(group.kind, ConversationKind::Group);
        assert_eq!(group.topic.as_deref(), Some("Projekt Tymy"));
        assert!(group.last_message.is_none(), "prázdný lastMessage → None");
        assert!(!group.has_unread());

        assert_eq!(convs[2].kind, ConversationKind::Meeting);
        assert_eq!(convs[3].kind, ConversationKind::System);
    }

    #[test]
    fn channels_know_their_team() {
        let raw = serde_json::json!({"conversations": [
            {"id": "19:root@thread.tacv2", "threadProperties": {"threadType": "space", "productThreadType": "TeamsTeam", "spaceThreadTopic": "Firma"}},
            {"id": "19:ch@thread.tacv2", "threadProperties": {"threadType": "topic", "topic": "Vývoj", "spaceId": "19:root@thread.tacv2"}},
            {"id": "19:g@thread.v2", "threadProperties": {"threadType": "chat", "topic": "Skupina"}}
        ]});
        let r: ConversationsResponse = serde_json::from_value(raw).unwrap();
        let c: Vec<_> = r
            .conversations
            .into_iter()
            .map(|c| c.into_domain())
            .collect();
        let root = c[0].team.as_ref().unwrap();
        assert_eq!(root.id.as_str(), "19:root@thread.tacv2");
        assert_eq!(root.name.as_deref(), Some("Firma"));
        assert_eq!(c[0].kind, ConversationKind::Channel);
        assert_eq!(
            c[1].team.as_ref().unwrap().id.as_str(),
            "19:root@thread.tacv2"
        );
        assert_eq!(c[1].topic.as_deref(), Some("Vývoj"));
        assert!(c[2].team.is_none());
    }

    #[test]
    fn left_teams_are_recognized() {
        let raw = serde_json::json!({"conversations": [
            {"id": "19:left@thread.tacv2", "threadProperties": {"threadType": "topic", "lastjoinat": "100", "lastleaveat": "200"}},
            {"id": "19:back@thread.tacv2", "threadProperties": {"threadType": "topic", "lastjoinat": 300, "lastleaveat": "200"}},
            {"id": "19:member@thread.tacv2", "threadProperties": {"threadType": "topic", "lastjoinat": "100"}},
            {"id": "19:chat@thread.v2", "threadProperties": {"threadType": "chat", "lastjoinat": "100", "lastleaveat": "200"}}
        ]});
        let r: ConversationsResponse = serde_json::from_value(raw).unwrap();
        let left: Vec<bool> = r.conversations.iter().map(|c| c.left_team()).collect();
        assert_eq!(
            left,
            [true, false, false, false],
            "opuštěný skupinový chat zůstává"
        );
    }

    #[test]
    fn team_channels_are_channels() {
        assert_eq!(
            conversation_kind("19:abc@thread.tacv2", None),
            ConversationKind::Channel
        );
        assert_eq!(
            conversation_kind("19:abc@thread.skype", None),
            ConversationKind::Channel
        );
    }

    #[test]
    fn parses_messages_with_files_edits_and_events() {
        let r: MessagesResponse = serde_json::from_str(&fixture("messages.json")).unwrap();
        assert!(r.metadata.backward_link.is_some());
        let msgs: Vec<_> = r
            .messages
            .into_iter()
            .filter_map(|m| m.into_domain(Some("19:fallback@thread.v2")))
            .collect();
        assert_eq!(msgs.len(), 3);

        let file_msg = &msgs[0];
        assert_eq!(file_msg.kind, MessageKind::RichText);
        assert_eq!(file_msg.attachments.len(), 1);
        assert_eq!(file_msg.attachments[0].name, "zprava.pdf");
        assert_eq!(file_msg.attachments[0].file_type.as_deref(), Some("pdf"));
        assert!(
            file_msg.attachments[0]
                .url
                .as_deref()
                .unwrap()
                .starts_with("https://")
        );
        assert!(file_msg.edited_at.is_some());
        assert_eq!(file_msg.reactions.len(), 1, "prázdná reakce se vynechá");
        assert_eq!(file_msg.reactions[0].key, "like");
        assert_eq!(file_msg.reactions[0].users.len(), 2);
        assert!(!file_msg.deleted);

        assert!(msgs[1].deleted);
        assert_eq!(
            msgs[2].kind,
            MessageKind::Event("ThreadActivity/AddMember".into())
        );
        assert_eq!(msgs[2].conversation_id.as_str(), "19:fallback@thread.v2");
    }

    #[test]
    fn parses_poll_events() {
        let r: PollResponse = serde_json::from_str(&fixture("poll.json")).unwrap();
        let events: Vec<_> = r
            .event_messages
            .into_iter()
            .map(|e| e.into_domain())
            .collect();
        assert_eq!(events.len(), 4);
        match &events[0] {
            Event::NewMessage(m) => {
                assert_eq!(m.conversation_id.as_str(), "19:abc@thread.v2");
                assert_eq!(m.content, "<p>Ahoj</p>");
            }
            e => panic!("{e:?}"),
        }
        match &events[1] {
            Event::Typing {
                conversation_id,
                user,
            } => {
                assert_eq!(conversation_id.as_str(), "19:abc@thread.v2");
                assert!(user.is_some());
            }
            e => panic!("{e:?}"),
        }
        assert_eq!(
            events[2],
            Event::ConversationUpdated("19:abc@thread.v2".into())
        );
        assert!(
            matches!(&events[3], Event::Unknown { resource_type, .. } if resource_type == "UserPresence")
        );
    }
}
