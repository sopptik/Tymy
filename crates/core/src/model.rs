use std::fmt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Typ Microsoft účtu. Liší se přihlašováním i hostiteli API.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AccountKind {
    /// Pracovní nebo školní účet (Microsoft 365, Entra ID), teams.microsoft.com
    Work,
    /// Osobní Microsoft účet (Teams free), teams.live.com
    Personal,
}

impl fmt::Display for AccountKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            AccountKind::Work => "work",
            AccountKind::Personal => "personal",
        })
    }
}

macro_rules! string_id {
    ($(#[$m:meta])* $name:ident) => {
        $(#[$m])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub String);

        impl $name {
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl From<&str> for $name {
            fn from(s: &str) -> Self {
                Self(s.to_owned())
            }
        }

        impl From<String> for $name {
            fn from(s: String) -> Self {
                Self(s)
            }
        }
    };
}

string_id!(
    /// Lokální identifikátor účtu (stabilní, např. object id uživatele z tokenu).
    AccountId
);
string_id!(
    /// MRI uživatele, např. `8:orgid:<uuid>` nebo `8:live:<jméno>`.
    UserId
);
string_id!(
    /// Id konverzace, např. `19:<...>@unq.gbl.spaces` nebo `19:<...>@thread.v2`.
    ConversationId
);
string_id!(
    /// Serverové id zprávy (milisekundový timestamp jako řetězec).
    MessageId
);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConversationKind {
    OneOnOne,
    Group,
    /// Kanál týmu; v první verzi jen pro čtení nebo skrytý.
    Channel,
    /// Chat schůzky.
    Meeting,
    /// Systémové konverzace (notifikace, "48:" vlákna apod.).
    System,
    Other(String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Conversation {
    pub id: ConversationId,
    pub kind: ConversationKind,
    /// Název skupinového chatu; u 1:1 obvykle chybí a jméno se odvozuje z členů.
    pub topic: Option<String>,
    pub last_message: Option<Message>,
    pub last_activity: Option<DateTime<Utc>>,
    /// Id poslední přečtené zprávy (consumption horizon).
    pub read_up_to: Option<MessageId>,
}

impl Conversation {
    /// Zobrazovat v seznamu chatů? (Skype-like: bez kanálů týmů a systémových vláken.)
    pub fn is_chat(&self) -> bool {
        !matches!(
            self.kind,
            ConversationKind::Channel | ConversationKind::System
        )
    }

    pub fn has_unread(&self) -> bool {
        match (&self.last_message, &self.read_up_to) {
            (Some(last), Some(read)) => message_id_newer(&last.id, read),
            (Some(_), None) => true,
            _ => false,
        }
    }

    /// Text vhodný pro zobrazení v seznamu chatů.
    pub fn display_title(&self) -> String {
        if let Some(t) = self.topic.as_deref().filter(|t| !t.trim().is_empty()) {
            return t.to_owned();
        }
        if let Some(name) = self
            .last_message
            .as_ref()
            .and_then(|m| m.sender_name.as_deref())
            .filter(|n| !n.is_empty())
        {
            return name.to_owned();
        }
        self.id.to_string()
    }
}

/// Id zpráv jsou čísla (ms timestamp); porovnává se numericky, při chybě lexikálně.
fn message_id_newer(a: &MessageId, b: &MessageId) -> bool {
    match (a.0.parse::<u64>(), b.0.parse::<u64>()) {
        (Ok(a), Ok(b)) => a > b,
        _ => a.0 > b.0,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MessageKind {
    /// Prostý text.
    Text,
    /// HTML obsah (výchozí formát Teams).
    RichText,
    /// Systémová událost (přidání člena, hovor, změna názvu, ...); obsahuje původní typ.
    Event(String),
    Unknown(String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub id: MessageId,
    pub conversation_id: ConversationId,
    /// Id přidělené klientem při odeslání; slouží k párování lokálně odeslané zprávy s ozvěnou ze serveru.
    pub client_message_id: Option<String>,
    pub sender: Option<UserId>,
    pub sender_name: Option<String>,
    pub kind: MessageKind,
    /// Surový obsah (HTML u `RichText`).
    pub content: String,
    pub composed_at: Option<DateTime<Utc>>,
    pub arrived_at: Option<DateTime<Utc>>,
    pub edited_at: Option<DateTime<Utc>>,
    pub deleted: bool,
    pub attachments: Vec<Attachment>,
}

impl Message {
    pub fn is_user_content(&self) -> bool {
        matches!(self.kind, MessageKind::Text | MessageKind::RichText)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attachment {
    pub name: String,
    /// Odkaz pro stažení / otevření (SharePoint/OneDrive u pracovních účtů, AMS u osobních).
    pub url: Option<String>,
    pub size: Option<u64>,
    /// Typ souboru, jak ho uvádí server (např. přípona `pdf`).
    pub file_type: Option<String>,
}

/// Událost doručená real-time kanálem.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Event {
    NewMessage(Message),
    MessageUpdated(Message),
    ConversationUpdated(ConversationId),
    Typing {
        conversation_id: ConversationId,
        user: Option<UserId>,
    },
    /// Událost, kterou zatím neumíme interpretovat. Necháváme surová data kvůli ladění protokolu.
    Unknown {
        resource_type: String,
        raw: serde_json::Value,
    },
}

/// Výsledek odeslání zprávy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SentMessage {
    pub client_message_id: String,
    /// Čas přijetí serverem, pokud ho server vrátil.
    pub arrived_at: Option<DateTime<Utc>>,
}

/// Přihlášený uživatel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Profile {
    pub account_id: AccountId,
    pub kind: AccountKind,
    pub display_name: Option<String>,
    pub username: Option<String>,
    pub tenant_id: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(id: &str) -> Message {
        Message {
            id: id.into(),
            conversation_id: "c".into(),
            client_message_id: None,
            sender: None,
            sender_name: Some("Alice".into()),
            kind: MessageKind::RichText,
            content: String::new(),
            composed_at: None,
            arrived_at: None,
            edited_at: None,
            deleted: false,
            attachments: vec![],
        }
    }

    fn conv(last: Option<&str>, read: Option<&str>) -> Conversation {
        Conversation {
            id: "19:abc@unq.gbl.spaces".into(),
            kind: ConversationKind::OneOnOne,
            topic: None,
            last_message: last.map(msg),
            last_activity: None,
            read_up_to: read.map(Into::into),
        }
    }

    #[test]
    fn unread_compares_numerically() {
        assert!(conv(Some("1000"), Some("999")).has_unread());
        assert!(!conv(Some("1000"), Some("1000")).has_unread());
        // lexikálně by "999" > "1000", numericky ne
        assert!(!conv(Some("999"), Some("1000")).has_unread());
        assert!(conv(Some("1"), None).has_unread());
        assert!(!conv(None, None).has_unread());
    }

    #[test]
    fn title_falls_back_to_sender_then_id() {
        let mut c = conv(Some("1"), None);
        assert_eq!(c.display_title(), "Alice");
        c.topic = Some("  ".into());
        assert_eq!(c.display_title(), "Alice");
        c.topic = Some("Projekt".into());
        assert_eq!(c.display_title(), "Projekt");
        c.topic = None;
        c.last_message = None;
        assert_eq!(c.display_title(), "19:abc@unq.gbl.spaces");
    }
}
