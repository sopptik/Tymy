//! Záznam hovoru v chatu (`Event/Call`): Teams ke každému hovoru posílá dvě zprávy,
//! `callStarted` (prázdný `partlist`) a `callEnded` (`<ended/>`, účastníci s délkou
//! v sekundách). Čas začátku je v `conversationStartTime` (UTC, `MM/DD/YYYY HH:MM:SS`);
//! `endTime` v `meetingDetails` patří k naplánované schůzce, ne k hovoru.

use chrono::{DateTime, NaiveDateTime, Utc};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallPhase {
    Started,
    Ended,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallLog {
    pub phase: CallPhase,
    /// Spojuje začátek a konec téhož hovoru.
    pub call_id: Option<String>,
    pub started_at: Option<DateTime<Utc>>,
    /// Délka hovoru (nejdelší účast) v sekundách; jen u konce.
    pub duration_secs: Option<u64>,
    /// Jména účastníků (u konce).
    pub participants: Vec<String>,
}

impl CallLog {
    /// Konec bez účasti protistrany (nikdo hovor nepřijal).
    pub fn missed(&self) -> bool {
        self.phase == CallPhase::Ended
            && (self.participants.len() < 2 || self.duration_secs.unwrap_or(0) == 0)
    }
}

/// Rozebere obsah zprávy `Event/Call`; `None`, když to není záznam hovoru.
pub fn parse(content: &str) -> Option<CallLog> {
    let event = tag(content, "callEventType");
    let phase = match event.as_deref() {
        Some("callEnded") => CallPhase::Ended,
        Some("callStarted") => CallPhase::Started,
        _ if content.contains("<ended/>") => CallPhase::Ended,
        _ if content.contains("<partlist") => CallPhase::Started,
        _ => return None,
    };
    let started_at = tag(content, "conversationStartTime").and_then(|t| {
        NaiveDateTime::parse_from_str(t.trim(), "%m/%d/%Y %H:%M:%S")
            .ok()
            .map(|n| n.and_utc())
    });
    let mut duration_secs: Option<u64> = None;
    let mut participants = vec![];
    let mut rest = content;
    while let Some(i) = rest.find("<part ") {
        let part = &rest[i..];
        let end = part.find("</part>").map_or(part.len(), |e| e + 7);
        let body = &part[..end];
        if let Some(d) = tag(body, "duration").and_then(|d| d.trim().parse::<u64>().ok()) {
            duration_secs = Some(duration_secs.map_or(d, |m| m.max(d)));
        }
        if let Some(n) = tag(body, "displayName")
            .filter(|n| !n.trim().is_empty())
            .or_else(|| tag(body, "name"))
        {
            participants.push(n);
        }
        rest = &part[end..];
    }
    Some(CallLog {
        phase,
        call_id: tag(content, "callId").filter(|c| !c.is_empty()),
        started_at,
        duration_secs,
        participants,
    })
}

/// Text prvního `<name>…</name>` (bez entit XML).
fn tag(s: &str, name: &str) -> Option<String> {
    let open = format!("<{name}>");
    let start = s.find(&open)? + open.len();
    let len = s[start..].find(&format!("</{name}>"))?;
    Some(unescape(&s[start..start + len]))
}

fn unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
}

/// Záznam z historie hovorů (`48:calllogs`). Hovory 1:1 se do chatu nezapisují, jen sem.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CallRecord {
    pub call_id: String,
    /// Protistrana (MRI).
    pub peer: crate::UserId,
    pub peer_name: Option<String>,
    pub incoming: bool,
    /// `accepted`, `missed`, `declined`, …
    pub state: String,
    /// `twoParty`, `groupCall`, …
    pub call_type: String,
    pub started_at: DateTime<Utc>,
    pub connected_at: Option<DateTime<Utc>>,
    pub ended_at: Option<DateTime<Utc>>,
}

impl CallRecord {
    /// Délka od spojení do konce (sekundy); `None` u nepřijatého hovoru.
    pub fn duration_secs(&self) -> Option<u64> {
        let (c, e) = (self.connected_at?, self.ended_at?);
        u64::try_from((e - c).num_seconds()).ok()
    }

    pub fn missed(&self) -> bool {
        self.state != "accepted" || self.connected_at.is_none()
    }
}

/// Délka česky: „45 s“, „3 min 12 s“, „1 h 05 min“.
pub fn format_duration(secs: u64) -> String {
    match secs {
        s if s < 60 => format!("{s} s"),
        s if s < 3600 => format!("{} min {} s", s / 60, s % 60),
        s => format!("{} h {:02} min", s / 3600, (s % 3600) / 60),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // smyšlená data ve tvaru zpráv Teams
    const ENDED: &str = "<ended/><partlist alt=\"\" count=\"2\">\
<part identity=\"8:orgid:aaaa\"><name>aaaa</name><displayName>Jana Nováková</displayName><duration>192</duration></part>\
<part identity=\"8:orgid:bbbb\"><name>bbbb</name><displayName>Petr &amp; spol.</displayName><duration>190</duration></part>\
</partlist><meetingDetails><meetingDetails><meetingType>Adhoc</meetingType>\
<startTime>01/01/0001 00:00:00</startTime><endTime>01/01/0001 00:00:00</endTime></meetingDetails>\
<callId>c-1</callId><callEventType>callEnded</callEventType>\
<conversationStartTime>03/14/2026 09:30:05</conversationStartTime></meetingDetails>";
    const STARTED: &str = "<partlist alt =\"\"></partlist><meetingDetails><meetingDetails>\
<meetingType>Adhoc</meetingType></meetingDetails><callId>c-1</callId>\
<callEventType>callStarted</callEventType>\
<conversationStartTime>03/14/2026 09:30:05</conversationStartTime></meetingDetails>";

    #[test]
    fn parses_ended_call() {
        let c = parse(ENDED).expect("hovor");
        assert_eq!(c.phase, CallPhase::Ended);
        assert_eq!(c.call_id.as_deref(), Some("c-1"));
        assert_eq!(c.duration_secs, Some(192));
        assert_eq!(c.participants, ["Jana Nováková", "Petr & spol."]);
        assert_eq!(
            c.started_at.unwrap().to_rfc3339(),
            "2026-03-14T09:30:05+00:00"
        );
        assert!(!c.missed());
    }

    #[test]
    fn parses_started_call() {
        let c = parse(STARTED).expect("hovor");
        assert_eq!(c.phase, CallPhase::Started);
        assert_eq!(c.duration_secs, None);
        assert!(c.participants.is_empty());
        assert!(parse("<p>ahoj</p>").is_none());
    }

    #[test]
    fn formats_duration() {
        assert_eq!(format_duration(45), "45 s");
        assert_eq!(format_duration(192), "3 min 12 s");
        assert_eq!(format_duration(3900), "1 h 05 min");
    }
}
