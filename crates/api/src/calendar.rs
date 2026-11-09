//! Kalendář přes middle tier Teams (`…/api/mt/<region>/v2.1/me/calendars/default/calendarView`).
//!
//! Stejné přihlášení jako chat (bearer `skype_scope` + skypetoken). Ověřeno 2026-11-09 na dvou
//! pracovních účtech (tymy-cli mt). Odpověď: `{"count": n, "value": [událost…]}`, časy v UTC
//! s posunem (`2026-11-09T09:00:00+00:00`), opakované schůzky jako jednotlivé výskyty.

use chrono::{DateTime, Utc};
use reqwest::Method;
use serde::Deserialize;
use serde_json::Value;
use tymy_core::{CalendarEvent, Error, EventResponse, Result, ShowAs};

use crate::auth::SkypeSession;
use crate::http::Http;
use crate::people::mt_request;

/// Události zasahující do intervalu `from..to`, seřazené podle začátku.
pub async fn calendar_view(
    http: &Http,
    session: &SkypeSession,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> Result<Vec<CalendarEvent>> {
    let v = mt_request(http, session, Method::GET, &view_path(from, to), None).await?;
    parse_events(&v)
}

fn view_path(from: DateTime<Utc>, to: DateTime<Utc>) -> String {
    const FMT: &str = "%Y-%m-%dT%H:%M:%S%.3fZ";
    format!(
        "v2.1/me/calendars/default/calendarView?StartDate={}&EndDate={}&shouldDecryptData=true",
        from.format(FMT),
        to.format(FMT)
    )
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct WireEvent {
    object_id: Option<String>,
    subject: Option<String>,
    start_time: Option<DateTime<Utc>>,
    end_time: Option<DateTime<Utc>>,
    is_all_day_event: bool,
    location: Option<String>,
    organizer_name: Option<String>,
    body_preview: Option<String>,
    skype_teams_meeting_url: Option<String>,
    skype_teams_data_object: Option<WireTeamsData>,
    is_cancelled: bool,
    is_organizer: bool,
    my_response_type: Option<String>,
    show_as: Option<String>,
    reminder_minutes_before_start: Option<i64>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct WireTeamsData {
    cid: Option<String>,
}

fn parse_events(v: &Value) -> Result<Vec<CalendarEvent>> {
    let items = v
        .get("value")
        .and_then(Value::as_array)
        .ok_or_else(|| Error::Protocol("kalendář: odpověď bez pole value".into()))?;
    let mut out: Vec<CalendarEvent> = items
        .iter()
        .filter_map(|item| {
            let w: WireEvent = match serde_json::from_value(item.clone()) {
                Ok(w) => w,
                Err(e) => {
                    tracing::warn!(error = %e, "kalendář: nečitelná událost, přeskakuji");
                    return None;
                }
            };
            let (Some(id), Some(start), Some(end)) = (w.object_id, w.start_time, w.end_time) else {
                return None;
            };
            let text = |s: Option<String>| s.map(|s| s.trim().to_owned()).filter(|s| !s.is_empty());
            Some(CalendarEvent {
                id,
                subject: text(w.subject).unwrap_or_else(|| "(bez názvu)".into()),
                start,
                end: end.max(start),
                all_day: w.is_all_day_event,
                location: text(w.location),
                organizer: text(w.organizer_name),
                preview: text(w.body_preview),
                join_url: text(w.skype_teams_meeting_url).filter(|u| u.starts_with("https://")),
                chat: w
                    .skype_teams_data_object
                    .and_then(|d| text(d.cid))
                    .map(Into::into),
                cancelled: w.is_cancelled,
                response: response(w.my_response_type.as_deref(), w.is_organizer),
                show_as: show_as(w.show_as.as_deref()),
                is_organizer: w.is_organizer,
                reminder_minutes: w.reminder_minutes_before_start.filter(|m| *m >= 0),
            })
        })
        .collect();
    out.sort_by(|a, b| a.start.cmp(&b.start).then_with(|| a.end.cmp(&b.end)));
    Ok(out)
}

fn response(v: Option<&str>, organizer: bool) -> EventResponse {
    match v.map(str::to_ascii_lowercase).as_deref() {
        Some("organizer") => EventResponse::Organizer,
        Some("accepted") => EventResponse::Accepted,
        Some("tentativelyaccepted" | "tentative") => EventResponse::Tentative,
        Some("declined") => EventResponse::Declined,
        Some("notresponded") => EventResponse::NotResponded,
        _ if organizer => EventResponse::Organizer,
        _ => EventResponse::None,
    }
}

fn show_as(v: Option<&str>) -> ShowAs {
    match v.map(str::to_ascii_lowercase).as_deref() {
        Some("free") => ShowAs::Free,
        Some("tentative") => ShowAs::Tentative,
        Some("busy") => ShowAs::Busy,
        Some("oof" | "outofoffice") => ShowAs::OutOfOffice,
        Some("workingelsewhere") => ShowAs::WorkingElsewhere,
        _ => ShowAs::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use serde_json::json;

    #[test]
    fn builds_view_path() {
        let from = Utc.with_ymd_and_hms(2026, 11, 9, 0, 0, 0).unwrap();
        let to = Utc.with_ymd_and_hms(2026, 11, 23, 0, 0, 0).unwrap();
        assert_eq!(
            view_path(from, to),
            "v2.1/me/calendars/default/calendarView?StartDate=2026-11-09T00:00:00.000Z\
             &EndDate=2026-11-23T00:00:00.000Z&shouldDecryptData=true"
        );
    }

    #[test]
    fn parses_events_sorted() {
        // zkrácený tvar skutečné odpovědi (anonymizováno)
        let v = json!({"count": 3, "value": [
            {
                "objectId": "B", "subject": "Porada", "startTime": "2026-11-12T14:15:00+00:00",
                "endTime": "2026-11-12T15:30:00+00:00", "isAllDayEvent": false,
                "isCancelled": false, "myResponseType": "NotResponded", "showAs": "Tentative",
                "isOrganizer": false, "location": "", "skypeTeamsMeetingUrl": ""
            },
            {
                "objectId": "A", "subject": " Stand-up ", "startTime": "2026-11-09T09:00:00+00:00",
                "endTime": "2026-11-09T09:30:00+00:00", "isAllDayEvent": false,
                "isCancelled": false, "myResponseType": "Accepted", "showAs": "Busy",
                "isOrganizer": false, "organizerName": "Jan Novák", "location": "Microsoft Teams Meeting",
                "skypeTeamsMeetingUrl": "https://teams.microsoft.com/l/meetup-join/19%3ameeting_x%40thread.v2/0",
                "skypeTeamsDataObject": {"cid": "19:meeting_x@thread.v2", "private": false, "type": "Scheduled"},
                "eventType": "Occurrence", "categories": []
            },
            {"objectId": "C", "subject": "bez konce", "startTime": "2026-11-10T09:00:00+00:00"}
        ]});
        let events = parse_events(&v).unwrap();
        assert_eq!(events.len(), 2, "událost bez konce se přeskočí");
        let a = &events[0];
        assert_eq!(a.id, "A");
        assert_eq!(a.subject, "Stand-up");
        assert_eq!(a.start, Utc.with_ymd_and_hms(2026, 11, 9, 9, 0, 0).unwrap());
        assert_eq!(a.organizer.as_deref(), Some("Jan Novák"));
        assert_eq!(a.chat.as_ref().unwrap().as_str(), "19:meeting_x@thread.v2");
        assert!(
            a.join_url
                .as_deref()
                .unwrap()
                .starts_with("https://teams.microsoft.com/")
        );
        assert_eq!(a.response, EventResponse::Accepted);
        assert_eq!(a.show_as, ShowAs::Busy);
        let b = &events[1];
        assert_eq!(b.location, None, "prázdné místo = žádné");
        assert_eq!(b.join_url, None);
        assert_eq!(b.response, EventResponse::NotResponded);
        assert!(b.wants_reminder());
    }

    #[test]
    fn declined_and_cancelled_need_no_reminder() {
        let v = json!({"value": [
            {"objectId": "A", "startTime": "2026-11-09T09:00:00+00:00", "endTime": "2026-11-09T10:00:00+00:00",
             "myResponseType": "Declined"},
            {"objectId": "B", "startTime": "2026-11-09T09:00:00+00:00", "endTime": "2026-11-09T10:00:00+00:00",
             "isCancelled": true, "isOrganizer": true}
        ]});
        let events = parse_events(&v).unwrap();
        assert_eq!(events[0].subject, "(bez názvu)");
        assert!(!events[0].wants_reminder());
        assert!(!events[1].wants_reminder());
        assert_eq!(events[1].response, EventResponse::Organizer);
        assert!(parse_events(&json!({"error": "x"})).is_err());
    }
}
