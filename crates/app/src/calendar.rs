//! Kalendář v okně: připomínky událostí (kdy upozornit, co už bylo upozorněno) a popisky.

use std::collections::HashMap;
use std::path::PathBuf;

use chrono::{DateTime, Datelike, Duration, Local, NaiveDate, NaiveTime, TimeZone, Timelike, Utc};
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

/// Datum z formuláře: „10. 11. 2026“, „10.11.2026“, „10. 11.“ (letos) nebo „2026-11-10“.
pub fn parse_date(s: &str, today: NaiveDate) -> Option<NaiveDate> {
    let s = s.trim();
    if let Ok(d) = NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        return Some(d);
    }
    let parts: Vec<&str> = s
        .split('.')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .collect();
    let num = |i: usize| parts.get(i).and_then(|p| p.parse::<u32>().ok());
    match parts.len() {
        2 => NaiveDate::from_ymd_opt(today.year(), num(1)?, num(0)?),
        3 => NaiveDate::from_ymd_opt(num(2)? as i32, num(1)?, num(0)?),
        _ => None,
    }
}

/// Čas z formuláře: „9:30“, „09:30“, „9.30“ nebo „9“.
pub fn parse_time(s: &str) -> Option<NaiveTime> {
    let s = s.trim().replace('.', ":");
    let (h, m) = match s.split_once(':') {
        Some((h, m)) => (h.trim().parse().ok()?, m.trim().parse().ok()?),
        None => (s.parse().ok()?, 0),
    };
    NaiveTime::from_hms_opt(h, m, 0)
}

/// Místní datum a čas → UTC (při přechodu na letní čas první platný okamžik).
pub fn local_to_utc(day: NaiveDate, time: NaiveTime) -> Option<DateTime<Utc>> {
    Local
        .from_local_datetime(&day.and_time(time))
        .earliest()
        .map(|t| t.with_timezone(&Utc))
}

/// Výchozí čas nové schůzky: nejbližší celá půlhodina od `now`, na 30 minut.
pub fn default_slot(now: DateTime<Utc>) -> (NaiveDate, NaiveTime, NaiveTime) {
    let local = now.with_timezone(&Local);
    let mins = local.hour() * 60 + local.minute();
    let start = (mins / 30 + 1) * 30;
    let start_dt = local.date_naive().and_time(NaiveTime::MIN) + Duration::minutes(start as i64);
    let end_dt = start_dt + Duration::minutes(30);
    (start_dt.date(), start_dt.time(), end_dt.time())
}

/// Pondělí týdne, do kterého patří `day`.
pub fn week_start(day: NaiveDate) -> NaiveDate {
    day - Duration::days(i64::from(day.weekday().num_days_from_monday()))
}

/// Interval týdne od pondělní místní půlnoci (pro načtení z kalendáře).
pub fn week_range(monday: NaiveDate) -> (DateTime<Utc>, DateTime<Utc>) {
    (
        local_midnight(monday),
        local_midnight(monday + Duration::days(7)),
    )
}

/// Nadpis týdne: „9.–15. 11. 2026“, přes měsíc „28. 9.–4. 10. 2026“.
pub fn week_label(monday: NaiveDate) -> String {
    let sunday = monday + Duration::days(6);
    if monday.month() == sunday.month() {
        format!(
            "{}.–{}. {}. {}",
            monday.day(),
            sunday.day(),
            sunday.month(),
            sunday.year()
        )
    } else if monday.year() == sunday.year() {
        format!(
            "{}. {}.–{}. {}. {}",
            monday.day(),
            monday.month(),
            sunday.day(),
            sunday.month(),
            sunday.year()
        )
    } else {
        format!(
            "{}. {}. {}–{}. {}. {}",
            monday.day(),
            monday.month(),
            monday.year(),
            sunday.day(),
            sunday.month(),
            sunday.year()
        )
    }
}

/// Zkratka dne v týdnu (0 = pondělí).
pub fn weekday_short(i: usize) -> &'static str {
    WEEKDAYS[i % 7]
}

/// Blok události v týdenní mřížce: den (0 = pondělí), minuty od půlnoci a sloupec
/// mezi souběžnými událostmi (`lane` z `lanes`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    pub event: usize,
    pub day: usize,
    pub from: i64,
    pub to: i64,
    pub lane: usize,
    pub lanes: usize,
}

/// Nejkratší zobrazená délka bloku (krátké události by nešly přečíst).
const MIN_BLOCK_MINUTES: i64 = 20;

/// Rozloží události do týdne: celodenní (index, první a poslední den v týdnu) a bloky
/// časovaných událostí po dnech. Událost přes půlnoc má blok v každém dni. Souběžné
/// události se dělí o šířku dne (sloupce přidělené zleva podle začátku).
pub fn week_layout(
    events: &[CalendarEvent],
    monday: NaiveDate,
) -> (Vec<(usize, usize, usize)>, Vec<Block>) {
    let mut all_day = vec![];
    let mut blocks: Vec<Block> = vec![];
    for (i, e) in events.iter().enumerate() {
        if e.all_day {
            let first = (start_day(e) - monday).num_days();
            let last = (last_day(e) - monday).num_days();
            if last >= 0 && first <= 6 {
                all_day.push((i, first.max(0) as usize, last.min(6) as usize));
            }
            continue;
        }
        for d in 0..7 {
            let day_start = local_midnight(monday + Duration::days(d));
            let day_end = local_midnight(monday + Duration::days(d + 1));
            if e.start >= day_end || e.end.max(e.start + Duration::minutes(1)) <= day_start {
                continue;
            }
            let from = (e.start.max(day_start) - day_start).num_minutes();
            let to = (e.end.min(day_end) - day_start).num_minutes();
            let day_len = (day_end - day_start).num_minutes();
            let to = to.max(from + MIN_BLOCK_MINUTES).min(day_len);
            let from = from.min(to - 1);
            blocks.push(Block {
                event: i,
                day: d as usize,
                from,
                to,
                lane: 0,
                lanes: 1,
            });
        }
    }
    // sloupce souběžných událostí: po dnech, shluky navzájem se překrývajících
    blocks.sort_by_key(|b| (b.day, b.from, std::cmp::Reverse(b.to)));
    let mut i = 0;
    while i < blocks.len() {
        let mut j = i;
        let mut cluster_end = blocks[i].to;
        let mut lane_ends: Vec<i64> = vec![];
        while j < blocks.len() && blocks[j].day == blocks[i].day && blocks[j].from < cluster_end {
            let lane = match lane_ends.iter().position(|end| *end <= blocks[j].from) {
                Some(l) => {
                    lane_ends[l] = blocks[j].to;
                    l
                }
                None => {
                    lane_ends.push(blocks[j].to);
                    lane_ends.len() - 1
                }
            };
            blocks[j].lane = lane;
            cluster_end = cluster_end.max(blocks[j].to);
            j += 1;
        }
        for b in &mut blocks[i..j] {
            b.lanes = lane_ends.len();
        }
        i = j;
    }
    (all_day, blocks)
}

/// Krok výběru času tažením v mřížce (minuty).
pub const DRAG_STEP: i64 = 15;

/// Výběr času tažením myši: od kotvy (stisk) po aktuální pozici, v minutách od půlnoci.
/// Zaokrouhleno na `DRAG_STEP` ven; pouhé kliknutí = 30 minut od kliknuté čtvrthodiny.
pub fn drag_range(anchor: i64, current: i64) -> (i64, i64) {
    let (a, b) = (anchor.min(current), anchor.max(current));
    let from = (a / DRAG_STEP * DRAG_STEP).clamp(0, 24 * 60 - DRAG_STEP);
    let to = ((b + DRAG_STEP - 1) / DRAG_STEP * DRAG_STEP).clamp(0, 24 * 60);
    // myš se skoro nepohnula = klik
    if b - a < DRAG_STEP / 2 || to - from < DRAG_STEP {
        (from, (from + 30).min(24 * 60))
    } else {
        (from, to)
    }
}

/// Minuty od půlnoci jako čas (24:00 → 23:59, schůzka musí skončit týž den).
pub fn minutes_time(m: i64) -> NaiveTime {
    let m = m.clamp(0, 24 * 60 - 1) as u32;
    NaiveTime::from_hms_opt(m / 60, m % 60, 0).unwrap_or(NaiveTime::MIN)
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
    fn parses_form_date_and_time() {
        let today = NaiveDate::from_ymd_opt(2026, 11, 9).unwrap();
        let d = |y, m, dd| NaiveDate::from_ymd_opt(y, m, dd);
        assert_eq!(parse_date("10. 11. 2026", today), d(2026, 11, 10));
        assert_eq!(parse_date("10.11.2026", today), d(2026, 11, 10));
        assert_eq!(parse_date("3. 1.", today), d(2026, 1, 3));
        assert_eq!(parse_date("2026-12-24", today), d(2026, 12, 24));
        assert_eq!(parse_date("31. 2. 2026", today), None);
        assert_eq!(parse_date("zítra", today), None);
        let t = |h, m| NaiveTime::from_hms_opt(h, m, 0);
        assert_eq!(parse_time("9:30"), t(9, 30));
        assert_eq!(parse_time(" 09.05 "), t(9, 5));
        assert_eq!(parse_time("14"), t(14, 0));
        assert_eq!(parse_time("25:00"), None);
        assert_eq!(parse_time("x"), None);
    }

    #[test]
    fn default_slot_is_next_half_hour() {
        let now = local_to_utc(
            NaiveDate::from_ymd_opt(2026, 11, 9).unwrap(),
            NaiveTime::from_hms_opt(9, 10, 0).unwrap(),
        )
        .unwrap();
        let (day, start, end) = default_slot(now);
        assert_eq!(day, NaiveDate::from_ymd_opt(2026, 11, 9).unwrap());
        assert_eq!(start, NaiveTime::from_hms_opt(9, 30, 0).unwrap());
        assert_eq!(end, NaiveTime::from_hms_opt(10, 0, 0).unwrap());
        // před půlnocí přejde na další den
        let late = local_to_utc(
            NaiveDate::from_ymd_opt(2026, 11, 9).unwrap(),
            NaiveTime::from_hms_opt(23, 40, 0).unwrap(),
        )
        .unwrap();
        assert_eq!(
            default_slot(late).0,
            NaiveDate::from_ymd_opt(2026, 11, 10).unwrap()
        );
    }

    fn local(d: u32, h: u32, m: u32) -> DateTime<Utc> {
        local_to_utc(
            NaiveDate::from_ymd_opt(2026, 11, d).unwrap(),
            NaiveTime::from_hms_opt(h, m, 0).unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn week_helpers() {
        let thu = NaiveDate::from_ymd_opt(2026, 11, 12).unwrap();
        let mon = week_start(thu);
        assert_eq!(mon, NaiveDate::from_ymd_opt(2026, 11, 9).unwrap());
        assert_eq!(week_start(mon), mon);
        assert_eq!(week_label(mon), "9.–15. 11. 2026");
        assert_eq!(
            week_label(NaiveDate::from_ymd_opt(2026, 9, 28).unwrap()),
            "28. 9.–4. 10. 2026"
        );
        assert_eq!(
            week_label(NaiveDate::from_ymd_opt(2026, 12, 28).unwrap()),
            "28. 12. 2026–3. 1. 2027"
        );
        let (from, to) = week_range(mon);
        assert_eq!((to - from).num_days(), 7);
    }

    #[test]
    fn lays_out_week_with_overlaps() {
        let mon = NaiveDate::from_ymd_opt(2026, 11, 9).unwrap();
        let ev = |id: &str, s: DateTime<Utc>, e: DateTime<Utc>| {
            let mut x = event(id, s, 0);
            x.end = e;
            x
        };
        let events = vec![
            ev("a", local(9, 10, 0), local(9, 11, 0)),
            ev("b", local(9, 10, 30), local(9, 11, 30)),
            ev("c", local(9, 11, 0), local(9, 12, 0)),
            ev("d", local(9, 13, 0), local(9, 13, 5)),
            ev("noc", local(10, 23, 0), local(11, 1, 0)),
            ev("mimo", local(16, 9, 0), local(16, 10, 0)),
        ];
        let (all_day, blocks) = week_layout(&events, mon);
        assert!(all_day.is_empty());
        let find = |i: usize| {
            blocks
                .iter()
                .filter(|b| b.event == i)
                .cloned()
                .collect::<Vec<_>>()
        };
        let (a, b, c) = (&find(0)[0], &find(1)[0], &find(2)[0]);
        assert_eq!((a.day, a.from, a.to), (0, 600, 660));
        assert_eq!(
            (a.lane, b.lane, c.lane),
            (0, 1, 0),
            "c navazuje na a, b vedle"
        );
        assert_eq!((a.lanes, b.lanes, c.lanes), (2, 2, 2));
        let d = &find(3)[0];
        assert_eq!(
            (d.from, d.to, d.lanes),
            (780, 800, 1),
            "krátká událost má min. 20 min"
        );
        let night = find(4);
        assert_eq!(night.len(), 2, "přes půlnoc ve dvou dnech");
        assert_eq!((night[0].day, night[0].from, night[0].to), (1, 1380, 1440));
        assert_eq!((night[1].day, night[1].from, night[1].to), (2, 0, 60));
        assert!(find(5).is_empty(), "další týden");
    }

    #[test]
    fn drag_ranges_snap_to_quarters() {
        assert_eq!(drag_range(605, 605), (600, 630), "klik = 30 min");
        assert_eq!(drag_range(600, 668), (600, 675));
        assert_eq!(drag_range(668, 600), (600, 675), "táhnutí nahoru");
        assert_eq!(drag_range(1430, 1450), (1425, 1440));
        assert_eq!(drag_range(-5, 20), (0, 30));
        assert_eq!(
            minutes_time(630),
            NaiveTime::from_hms_opt(10, 30, 0).unwrap()
        );
        assert_eq!(
            minutes_time(1440),
            NaiveTime::from_hms_opt(23, 59, 0).unwrap()
        );
    }

    #[test]
    fn all_day_in_week() {
        let mon = NaiveDate::from_ymd_opt(2026, 11, 9).unwrap();
        let mut e = event(
            "x",
            local_midnight(NaiveDate::from_ymd_opt(2026, 11, 7).unwrap()),
            0,
        );
        e.all_day = true;
        e.end = local_midnight(NaiveDate::from_ymd_opt(2026, 11, 11).unwrap());
        let (all_day, blocks) = week_layout(&[e], mon);
        assert!(blocks.is_empty());
        assert_eq!(
            all_day,
            vec![(0, 0, 1)],
            "od soboty do úterý → v týdnu Po–Út"
        );
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
