//! Události kalendáře (schůzky, připomínky, celodenní události).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ConversationId;

/// Jedna událost (u opakovaných jeden výskyt).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CalendarEvent {
    /// Id výskytu (u opakované události se liší pro každý výskyt).
    pub id: String,
    pub subject: String,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    /// Celodenní událost: `start`/`end` jsou půlnoci v časovém pásmu události.
    pub all_day: bool,
    pub location: Option<String>,
    pub organizer: Option<String>,
    /// Začátek textu pozvánky (prostý text).
    pub preview: Option<String>,
    /// Odkaz pro připojení k online schůzce.
    pub join_url: Option<String>,
    /// Chat schůzky.
    pub chat: Option<ConversationId>,
    pub cancelled: bool,
    pub response: EventResponse,
    pub show_as: ShowAs,
    pub is_organizer: bool,
    /// Připomenutí v minutách před začátkem, pokud ho server uvádí.
    pub reminder_minutes: Option<i64>,
}

impl CalendarEvent {
    /// Má smysl na událost upozorňovat (nezrušená, neodmítnutá)?
    pub fn wants_reminder(&self) -> bool {
        !self.cancelled && self.response != EventResponse::Declined
    }
}

/// Nová schůzka k naplánování.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewMeeting {
    pub subject: String,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub attendees: Vec<MeetingAttendee>,
    /// Teams schůzka (odkaz „Připojit“).
    pub online: bool,
    pub body: Option<String>,
    pub location: Option<String>,
    pub reminder_minutes: i64,
}

/// Pozvaný účastník (e-mail je nutný pro pozvánku).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MeetingAttendee {
    pub email: String,
    pub name: String,
}

/// Odpověď na pozvánku.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventReply {
    Accept,
    Tentative,
    Decline,
}

/// Moje odpověď na pozvánku.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum EventResponse {
    #[default]
    None,
    Organizer,
    Accepted,
    Tentative,
    Declined,
    NotResponded,
}

/// Jak se událost projeví v dostupnosti.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ShowAs {
    Free,
    Tentative,
    #[default]
    Busy,
    OutOfOffice,
    WorkingElsewhere,
    Unknown,
}
