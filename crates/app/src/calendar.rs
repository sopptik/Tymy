//! Kalendář v okně: připomínky událostí (kdy upozornit, co už bylo upozorněno) a popisky.

use std::collections::HashMap;
use std::path::PathBuf;

use chrono::{DateTime, Datelike, Duration, Local, NaiveDate, NaiveTime, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use tymy_core::{AccountId, CalendarEvent, EventResponse};

/// Kolik dní dopředu načítat (od dnešní půlnoci).
pub const DAYS_AHEAD: i64 = 14;
/// O kolik odložit připomínku.
pub const SNOOZE_MINUTES: i64 = 5;
/// V kolik hodin (místního času) připomenout celodenní událost.
const ALL_DAY_HOUR: u32 = 8;

/// Interval, který kalendář načítá: od dnešní místní půlnoci `DAYS_AHEAD` dní dopředu.
pub fn fetch_range(now: DateTime<Utc>) -> (DateTime<Utc>, DateTime<Utc>) {
    let today = now.with_timezone(&Local).date_naive();
    let from = local_midnight(today);
    (from, from + Duration::days(DAYS_AHEAD))
}

fn local_midnight(day: NaiveDate) -> DateTime<Utc> {
    Local
        .from_local_datetime(&day.and_time(NaiveTime::MIN))
        .earliest()
        .map(|t| t.with_timezone(&Utc))
        .unwrap_or_else(|| day.and_time(NaiveTime::MIN).and_utc())
}

/// Místní den, kterým událost začíná. Celodenní události jsou půlnoci v pásmu události
/// (v UTC třeba 23:00 předchozího dne), proto se zaokrouhlí na nejbližší půlnoc.
pub fn start_day(e: &CalendarEvent) -> NaiveDate {
    if e.all_day {
        (e.start + Duration::hours(12))
            .with_timezone(&Local)
            .date_naive()
    } else {
        e.start.with_timezone(&Local).date_naive()
    }
}

/// Poslední místní den celodenní události (konec je výlučný).
fn last_day(e: &CalendarEvent) -> NaiveDate {
    let end = (e.end + Duration::hours(12))
        .with_timezone(&Local)
        .date_naive();
    end.pred_opt().unwrap_or(end).max(start_day(e))
}

/// Kdy na událost upozornit: `minutes` před začátkem (nebo podle serveru), celodenní
/// v `ALL_DAY_HOUR` hodin prvního dne.
pub fn remind_at(e: &CalendarEvent, minutes: i64) -> DateTime<Utc> {
    if e.all_day {
        let day = start_day(e);
        let at =
            day.and_time(NaiveTime::from_hms_opt(ALL_DAY_HOUR, 0, 0).unwrap_or(NaiveTime::MIN));
        return Local
            .from_local_datetime(&at)
            .earliest()
            .map(|t| t.with_timezone(&Utc))
            .unwrap_or(e.start);
    }
    e.start - Duration::minutes(e.reminder_minutes.unwrap_or(minutes))
}

/// Klíč připomínky: účet, výskyt a jeho začátek (přesunutá schůzka se připomene znovu).
pub fn key(account: &AccountId, e: &CalendarEvent) -> String {
    format!("{account}|{}|{}", e.id, e.start.timestamp())
}

/// Stav jedné připomínky: zobrazená (a do kdy si ji pamatovat), případně odložená.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct Entry {
    /// Konec události; potom se záznam zahodí.
    expires: DateTime<Utc>,
    /// Odloženo do; `None` = vyřízeno.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    snoozed_until: Option<DateTime<Utc>>,
    /// Připomenuto, ale uživatel si událost ještě neotevřel (ikona v liště ji ukazuje
    /// i po restartu aplikace).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pending: bool,
}

/// Které připomínky už byly zobrazené. Ukládá se na disk, aby se po restartu neopakovaly.
#[derive(Debug, Default)]
pub struct Reminders {
    entries: HashMap<String, Entry>,
    path: Option<PathBuf>,
}

impl Reminders {
    /// Načte stav z `~/.local/state/tymy/calendar-reminders.json` (chyby = prázdný stav).
    pub fn load() -> Self {
        let path = directories::ProjectDirs::from("", "", "tymy").map(|d| {
            d.state_dir()
                .unwrap_or_else(|| d.data_local_dir())
                .join("calendar-reminders.json")
        });
        let entries = path
            .as_ref()
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        Self { entries, path }
    }

    fn save(&self) {
        let Some(path) = &self.path else { return };
        let result = (|| -> std::io::Result<()> {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let tmp = path.with_extension("json.tmp");
            std::fs::write(&tmp, serde_json::to_vec(&self.entries)?)?;
            std::fs::rename(tmp, path)
        })();
        if let Err(e) = result {
            tracing::warn!(error = %e, "uložení stavu připomínek");
        }
    }

    /// Události, na které je teď čas upozornit, a rovnou je označí jako zobrazené.
    /// Upozorňuje se od `remind_at` do konce události (i po probuzení z uspání nebo při
    /// spuštění během schůzky), každá jednou; odložená znovu po uplynutí odkladu.
    pub fn take_due<'a>(
        &mut self,
        events: impl IntoIterator<Item = (&'a AccountId, &'a CalendarEvent)>,
        now: DateTime<Utc>,
        minutes: i64,
    ) -> Vec<(AccountId, CalendarEvent)> {
        let before = self.entries.len();
        self.entries.retain(|_, e| e.expires > now);
        let mut changed = self.entries.len() != before;
        let mut due = vec![];
        for (account, e) in events {
            if !e.wants_reminder() || now < remind_at(e, minutes) || now >= e.end {
                continue;
            }
            let k = key(account, e);
            let fire = match self.entries.get(&k) {
                None => true,
                Some(entry) => entry.snoozed_until.is_some_and(|t| now >= t),
            };
            if fire {
                self.entries.insert(
                    k,
                    Entry {
                        expires: e.end,
                        snoozed_until: None,
                        pending: true,
                    },
                );
                changed = true;
                due.push((account.clone(), e.clone()));
            }
        }
        if changed {
            self.save();
        }
        due
    }

    /// Odloží připomínku: o `SNOOZE_MINUTES`, ale ne přes začátek události (pokud ještě
    /// nezačala), a nejdéle do jejího konce.
    pub fn snooze(&mut self, account: &AccountId, e: &CalendarEvent, now: DateTime<Utc>) {
        let mut until = now + Duration::minutes(SNOOZE_MINUTES);
        if now < e.start && !e.all_day {
            until = until.min(e.start);
        }
        if until >= e.end {
            return;
        }
        self.entries.insert(
            key(account, e),
            Entry {
                expires: e.end,
                snoozed_until: Some(until),
                pending: true,
            },
        );
        self.save();
    }

    /// Byla událost připomenuta a uživatel si ji ještě neotevřel?
    pub fn is_pending(&self, account: &AccountId, e: &CalendarEvent) -> bool {
        self.entries
            .get(&key(account, e))
            .is_some_and(|x| x.pending)
    }

    /// Uživatel si událost otevřel (všechny její výskyty se stejným id).
    pub fn ack(&mut self, account: &AccountId, event_id: &str) {
        let prefix = format!("{account}|{event_id}|");
        let mut changed = false;
        for (k, entry) in &mut self.entries {
            if entry.pending && k.starts_with(&prefix) {
                entry.pending = false;
                changed = true;
            }
        }
        if changed {
            self.save();
        }
    }
}

const WEEKDAYS: [&str; 7] = ["Po", "Út", "St", "Čt", "Pá", "So", "Ne"];

/// Nadpis dne v agendě: „Dnes“, „Zítra“, jinak „St 11. 11.“.
pub fn day_label(day: NaiveDate, today: NaiveDate) -> String {
    let rel = match (day - today).num_days() {
        0 => Some("Dnes"),
        1 => Some("Zítra"),
        -1 => Some("Včera"),
        _ => None,
    };
    let date = format!(
        "{} {}. {}.",
        WEEKDAYS[day.weekday().num_days_from_monday() as usize],
        day.day(),
        day.month()
    );
    match rel {
        Some(r) => format!("{r} · {date}"),
        None => date,
    }
}

/// Čas události: „09:00–09:30“, přes půlnoc s datem konce, celodenní „Celý den“.
pub fn time_label(e: &CalendarEvent) -> String {
    if e.all_day {
        let (first, last) = (start_day(e), last_day(e));
        return if first == last {
            "Celý den".into()
        } else {
            format!("Celý den, do {}. {}.", last.day(), last.month())
        };
    }
    let (s, t) = (e.start.with_timezone(&Local), e.end.with_timezone(&Local));
    if s.date_naive() == t.date_naive() {
        format!("{}–{}", s.format("%H:%M"), t.format("%H:%M"))
    } else {
        format!(
            "{}–{}. {}. {}",
            s.format("%H:%M"),
            t.day(),
            t.month(),
            t.format("%H:%M")
        )
    }
}

/// Krátce, kdy událost je vzhledem k `now`: „v 10:00 (za 12 min)“, „probíhá od 10:00“,
/// celodenní „celý den“.
pub fn when_label(e: &CalendarEvent, now: DateTime<Utc>) -> String {
    if e.all_day {
        return time_label(e).to_lowercase();
    }
    let start = e.start.with_timezone(&Local).format("%H:%M");
    let secs = (e.start - now).num_seconds();
    let mins = secs.div_euclid(60) + i64::from(secs.rem_euclid(60) > 0);
    match mins {
        m if m > 60 => format!("v {start}"),
        m if m > 0 => format!("v {start} (za {m} min)"),
        0 => format!("v {start} (začíná)"),
        _ => format!("probíhá od {start}"),
    }
}

/// Text připomínky: kdy událost začíná vzhledem k `now` a kde.
pub fn reminder_body(e: &CalendarEvent, now: DateTime<Utc>) -> String {
    let when = if e.all_day {
        time_label(e)
    } else {
        let mins = (e.start - now).num_seconds().div_euclid(60)
            + i64::from((e.start - now).num_seconds().rem_euclid(60) > 0);
        let start = e.start.with_timezone(&Local).format("%H:%M");
        match mins {
            m if m > 60 => format!("V {start} ({})", time_label(e)),
            m if m > 0 => format!("Za {m} min ({})", time_label(e)),
            0 => format!("Začíná teď ({})", time_label(e)),
            _ => format!("Probíhá od {start} ({})", time_label(e)),
        }
    };
    let mut lines = vec![when];
    if let Some(l) = &e.location {
        lines.push(l.clone());
    }
    lines.join("\n")
}

/// Moje odpověď lidsky.
pub fn response_label(e: &CalendarEvent) -> Option<&'static str> {
    Some(match e.response {
        EventResponse::Organizer => "Jste organizátor",
        EventResponse::Accepted => "Přijato",
        EventResponse::Tentative => "Předběžně přijato",
        EventResponse::Declined => "Odmítnuto",
        EventResponse::NotResponded => "Bez odpovědi",
        EventResponse::None => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tymy_core::ShowAs;

    fn at(h: u32, m: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 9, h, m, 0).unwrap()
    }

    fn event(id: &str, start: DateTime<Utc>, mins: i64) -> CalendarEvent {
        CalendarEvent {
            id: id.into(),
            subject: "Porada".into(),
            start,
            end: start + Duration::minutes(mins),
            all_day: false,
            location: None,
            organizer: None,
            preview: None,
            join_url: None,
            chat: None,
            cancelled: false,
            response: EventResponse::Accepted,
            show_as: ShowAs::Busy,
            is_organizer: false,
            reminder_minutes: None,
        }
    }

    fn reminders() -> Reminders {
        Reminders::default() // bez cesty = nic se neukládá
    }

    #[test]
    fn reminds_once_from_remind_time_until_end() {
        let acc: AccountId = "a".into();
        let e = event("1", at(10, 0), 30);
        let mut r = reminders();
        let evs = [(&acc, &e)];
        assert!(r.take_due(evs, at(9, 44), 15).is_empty(), "ještě je brzy");
        assert_eq!(r.take_due(evs, at(9, 45), 15).len(), 1);
        assert!(r.take_due(evs, at(9, 50), 15).is_empty(), "jen jednou");
        // spuštění během schůzky: upozornit, po konci už ne
        let mut r = reminders();
        assert_eq!(r.take_due(evs, at(10, 20), 15).len(), 1);
        let mut r = reminders();
        assert!(r.take_due(evs, at(10, 30), 15).is_empty());
    }

    #[test]
    fn server_reminder_and_moved_event() {
        let acc: AccountId = "a".into();
        let mut e = event("1", at(10, 0), 30);
        e.reminder_minutes = Some(5);
        let mut r = reminders();
        assert!(r.take_due([(&acc, &e)], at(9, 50), 15).is_empty());
        assert_eq!(r.take_due([(&acc, &e)], at(9, 55), 15).len(), 1);
        // přesunutá schůzka = nový klíč = nová připomínka
        e.start = at(11, 0);
        e.end = at(11, 30);
        assert_eq!(r.take_due([(&acc, &e)], at(10, 56), 15).len(), 1);
    }

    #[test]
    fn skips_declined_and_cancelled() {
        let acc: AccountId = "a".into();
        let mut d = event("1", at(10, 0), 30);
        d.response = EventResponse::Declined;
        let mut c = event("2", at(10, 0), 30);
        c.cancelled = true;
        let mut r = reminders();
        assert!(
            r.take_due([(&acc, &d), (&acc, &c)], at(9, 50), 15)
                .is_empty()
        );
    }

    #[test]
    fn snooze_fires_again_but_not_past_start() {
        let acc: AccountId = "a".into();
        let e = event("1", at(10, 0), 30);
        let mut r = reminders();
        assert_eq!(r.take_due([(&acc, &e)], at(9, 57), 15).len(), 1);
        r.snooze(&acc, &e, at(9, 57));
        assert!(r.take_due([(&acc, &e)], at(9, 59), 15).is_empty());
        assert_eq!(
            r.take_due([(&acc, &e)], at(10, 0), 15).len(),
            1,
            "odloženo do začátku"
        );
        assert!(r.take_due([(&acc, &e)], at(10, 1), 15).is_empty());
        // odklad přes konec události se nezapíše
        r.snooze(&acc, &e, at(10, 28));
        assert!(r.take_due([(&acc, &e)], at(10, 29), 15).is_empty());
    }

    #[test]
    fn pending_until_acknowledged() {
        let acc: AccountId = "a".into();
        let e = event("1", at(10, 0), 30);
        let mut r = reminders();
        assert!(!r.is_pending(&acc, &e));
        r.take_due([(&acc, &e)], at(9, 50), 15);
        assert!(r.is_pending(&acc, &e));
        r.ack(&acc, "1");
        assert!(!r.is_pending(&acc, &e));
        assert!(
            r.take_due([(&acc, &e)], at(9, 55), 15).is_empty(),
            "potvrzení není nová připomínka"
        );
        // starý záznam bez pole `pending` se načte jako vyřízený
        let old: HashMap<String, Entry> =
            serde_json::from_str(r#"{"k":{"expires":"2026-11-09T09:30:00Z"}}"#).unwrap();
        assert!(!old["k"].pending);
    }

    #[test]
    fn forgets_finished_events() {
        let acc: AccountId = "a".into();
        let e = event("1", at(10, 0), 30);
        let mut r = reminders();
        r.take_due([(&acc, &e)], at(9, 50), 15);
        assert_eq!(r.entries.len(), 1);
        r.take_due(std::iter::empty(), at(10, 31), 15);
        assert!(r.entries.is_empty());
    }

    #[test]
    fn labels() {
        let today = NaiveDate::from_ymd_opt(2026, 11, 9).unwrap();
        assert_eq!(day_label(today, today), "Dnes · Po 9. 11.");
        assert_eq!(
            day_label(today.succ_opt().unwrap(), today),
            "Zítra · Út 10. 11."
        );
        assert_eq!(
            day_label(NaiveDate::from_ymd_opt(2026, 11, 13).unwrap(), today),
            "Pá 13. 11."
        );
        let e = event("1", at(10, 0), 30);
        let s = e.start.with_timezone(&Local).format("%H:%M").to_string();
        assert!(time_label(&e).starts_with(&s));
        assert!(reminder_body(&e, at(9, 45)).starts_with("Za 15 min"));
        assert!(reminder_body(&e, at(10, 0)).starts_with("Začíná teď"));
        assert!(reminder_body(&e, at(10, 5)).starts_with("Probíhá od"));
        let start = e.start.with_timezone(&Local).format("%H:%M").to_string();
        assert_eq!(when_label(&e, at(9, 48)), format!("v {start} (za 12 min)"));
        assert_eq!(when_label(&e, at(10, 1)), format!("probíhá od {start}"));
    }

    #[test]
    fn all_day_events_use_local_days() {
        // celodenní událost je půlnoc v pásmu události; v UTC může být předchozí den
        let day = NaiveDate::from_ymd_opt(2026, 11, 10).unwrap();
        let start = local_midnight(day);
        let mut e = event("1", start, 24 * 60);
        e.all_day = true;
        assert_eq!(start_day(&e), day);
        assert_eq!(time_label(&e), "Celý den");
        e.end = start + Duration::days(3);
        assert_eq!(time_label(&e), "Celý den, do 12. 11.");
        let remind = remind_at(&e, 15).with_timezone(&Local);
        assert_eq!(remind.date_naive(), day);
        assert_eq!(remind.format("%H:%M").to_string(), "08:00");
    }
}
