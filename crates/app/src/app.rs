//! Hlavní okno: seznam chatů vlevo, vlákno zpráv vpravo (rozložení jako Skype).

use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Datelike, Local, Timelike, Utc};
use iced::widget::{
    Column, Id, button, column, container, image, mouse_area, operation, rich_text, row,
    scrollable, space, span, stack, text, text_editor, text_input,
};
use iced::{Border, Color, Element, Font, Length, Subscription, Task, Theme};
use tymy_api::auth::{self, BrowserLogin, CredentialStore, DeviceCode, SecretServiceStore};
use tymy_api::{Http, TeamsBackend, complete_login};
use tymy_config::{AccountRecord, CalendarSettings, Config, ThemeMode, autostart};
use tymy_core::text::rich::{self, Block, QuoteKind};
use tymy_core::text::{
    InlineImage, Part, REPLY_PREVIEW_CHARS, edited_html, message_parts, truncate_chars,
};
use tymy_core::{
    AccountId, AccountKind, CalendarEvent, ChatBackend, Conversation, ConversationId,
    ConversationKind, ConversationPage, Error, EventReply, EventResponse, MentionRef, Message,
    MessageId, MessageKind, MessagePage, Outgoing, OutgoingImage, Person, Profile, ReplyRef,
    one_on_one_id, one_on_one_peer,
};

use tymy_store::{HIT_END, HIT_START, SearchHit, Store};

use iced::widget::text::Wrapping;

use crate::calendar::{self as cal, Reminders};
use crate::clipboard::{self, Clip};
use crate::emoji;
use crate::mention;
use crate::notify::{self, ReminderChoice};
use crate::state::{
    PeerNames, channel_name, group_teams, is_empty_chat, is_me, is_unread, merge_messages,
    move_pin, my_mri, order_chats, toggle_reaction, unread_count,
};
use crate::style::{self, BOLD, SEMIBOLD, accent, muted};
use crate::tray::{self, TrayEvent, TrayHandle};

const COMPOSER: Id = Id::new("composer");
const GROUP_QUERY: Id = Id::new("group-query");
const MESSAGES: Id = Id::new("messages");
/// Dokud nemáme trouter (WebSocket), obnovujeme seznam chatů periodicky (jen nejnovější
/// stránku ke sloučení). Jednou za `FULL_REFRESH` znovu načteme všechny už načtené stránky
/// (zmizí opuštěné chaty); starší stránky se dočítají líně (posun na konec, Týmy, hledání).
const REFRESH: Duration = Duration::from_secs(5);
const FULL_REFRESH: Duration = Duration::from_secs(5 * 60);
/// Kolik chatů bez známého jména projít v historii (záloha, když profily nevyjdou).
const PEER_SCAN_LIMIT: usize = 25;
/// U kolika skupinových chatů bez názvu najednou zjišťovat členy.
const GROUP_MEMBERS_LIMIT: usize = 30;

/// Chyba převedená pro UI; `reauth` = je nutné se znovu přihlásit.
#[derive(Debug, Clone)]
pub struct Failure {
    text: String,
    reauth: bool,
    /// Server odpověděl 404 (např. vlákno nového chatu ještě neexistuje).
    not_found: bool,
    /// Server odpověděl 403 (např. konverzace, jejímž členem už uživatel není).
    forbidden: bool,
}

impl From<Error> for Failure {
    fn from(e: Error) -> Self {
        Self {
            reauth: matches!(e, Error::ReauthRequired(_)),
            not_found: matches!(e, Error::Http { status: 404, .. }),
            forbidden: matches!(e, Error::Http { status: 403, .. }),
            text: e.to_string(),
        }
    }
}

impl From<anyhow::Error> for Failure {
    fn from(e: anyhow::Error) -> Self {
        Self {
            text: format!("{e:#}"),
            reauth: false,
            not_found: false,
            forbidden: false,
        }
    }
}

#[derive(Clone)]
pub struct Session {
    profile: Profile,
    backend: Arc<TeamsBackend>,
    /// Lokální cache; bez ní aplikace funguje dál, jen bez offline historie.
    store: Option<Store>,
}

impl fmt::Debug for Session {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Session")
            .field("account", &self.profile.account_id)
            .finish()
    }
}

/// Rozpracovaný nový skupinový chat (výběr lidí a název).
#[derive(Default)]
pub struct GroupBuilder {
    topic: String,
    query: String,
    seq: u64,
    searching: bool,
    results: Vec<Person>,
    members: Vec<Person>,
}

/// Rozpracovaná nová schůzka (formulář místo detailu události).
pub struct MeetingForm {
    subject: String,
    date: String,
    start: String,
    end: String,
    location: String,
    body: String,
    online: bool,
    attendees: Vec<Person>,
    query: String,
    seq: u64,
    searching: bool,
    results: Vec<Person>,
    busy: bool,
    error: Option<String>,
}

/// Výběr času tažením myši v jednom dni týdenní mřížky (minuty od půlnoci).
#[derive(Debug, Clone, Copy)]
pub struct GridDrag {
    day: usize,
    anchor: i64,
    current: i64,
}

/// Pole formuláře nové schůzky.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MeetingField {
    Subject,
    Date,
    Start,
    End,
    Location,
    Body,
    Query,
}

/// Otevřené nastavení skupinového chatu: název, členové, přidání lidí, opuštění.
pub struct ChatPanel {
    conv: ConversationId,
    topic: String,
    query: String,
    seq: u64,
    searching: bool,
    results: Vec<Person>,
    /// Běží změna na serveru (tlačítka jsou neaktivní).
    busy: bool,
    confirm_leave: bool,
    /// Člen, u kterého se čeká na potvrzení odebrání (druhý klik).
    confirm_remove: Option<tymy_core::UserId>,
}

/// Skupinový chat připravený v okně; na serveru vznikne první zprávou.
#[derive(Clone)]
struct DraftGroup {
    members: Vec<tymy_core::UserId>,
    names: Vec<String>,
    topic: Option<String>,
}

/// Spojení neaktivního účtu (kvůli počtu nepřečtených).
#[derive(Clone)]
pub struct OtherBackend(Arc<TeamsBackend>);

impl fmt::Debug for OtherBackend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("OtherBackend")
    }
}

/// Stav neaktivního účtu: spojení, počet nepřečtených konverzací a poslední známé zprávy
/// v chatech (kvůli notifikacím; `None` = ještě nic, první dotaz jen zapamatuje stav).
#[derive(Default)]
struct OtherAccount {
    backend: Option<Arc<TeamsBackend>>,
    unread: Option<usize>,
    error: Option<String>,
    last_seen: Option<HashMap<ConversationId, u64>>,
}

/// Jak často zjišťovat nepřečtené u neaktivních účtů.
const OTHERS_REFRESH: Duration = Duration::from_secs(60);

/// Jak často kontrolovat připomínky událostí.
const CALENDAR_TICK: Duration = Duration::from_secs(30);
/// Po kolika minutách kalendář znovu načíst (podle hodin, ne monotónního času: po uspání
/// notebooku se tak načte hned).
const CALENDAR_REFRESH_MINUTES: i64 = 5;

/// Kalendáře všech účtů a stav připomínek. Na rozdíl od ostatního stavu přežije přepnutí
/// účtu (připomínky chodí ze všech účtů).
struct CalendarState {
    events: HashMap<AccountId, Vec<CalendarEvent>>,
    /// Kdy jsme naposledy načítali (i neúspěšně).
    fetched: HashMap<AccountId, DateTime<Utc>>,
    loading: HashSet<AccountId>,
    errors: HashMap<AccountId, String>,
    reminders: Reminders,
    settings: CalendarSettings,
    /// Událost otevřená v okně (id výskytu v aktivním účtu).
    selected: Option<String>,
    /// Událost k otevření po přepnutí účtu (klik na připomínku z jiného účtu).
    open_after_switch: Option<String>,
    /// Připomenuté schůzky, které si uživatel ještě neotevřel (ukazuje je ikona v liště,
    /// i když notifikace mezitím zmizela). Zmizí otevřením, připojením nebo koncem události.
    alerts: Vec<(AccountId, CalendarEvent)>,
}

impl CalendarState {
    fn load() -> Self {
        Self {
            events: HashMap::new(),
            fetched: HashMap::new(),
            loading: HashSet::new(),
            errors: HashMap::new(),
            reminders: Reminders::load(),
            settings: Config::load()
                .map(|c| c.settings.calendar)
                .unwrap_or_default(),
            selected: None,
            open_after_switch: None,
            alerts: vec![],
        }
    }
}

#[derive(Debug, Clone)]
pub enum Msg {
    Booted(Result<Option<Session>, Failure>),
    StartLogin(AccountKind),
    LoginCode(AccountKind, Result<DeviceCode, Failure>),
    /// Přihlášení dokončeno: id účtu.
    LoginDone(Result<AccountId, Failure>),
    /// Zpět z přihlášení dalšího účtu.
    CancelLogin,
    /// Přihlášení v prohlížeči (authorization code + PKCE).
    StartBrowserLogin(AccountKind),
    /// Text vložený do pole pro adresu z prohlížeče.
    BrowserLoginInput(String),
    /// Obsah schránky po návratu do okna (možná adresa z prohlížeče).
    LoginClipboard(Option<String>),
    /// Kód převzatý místním serverem.
    BrowserCode(Result<String, Failure>),
    /// Otevřít/zavřít menu účtů v hlavičce panelu.
    ToggleAccountMenu,
    SwitchAccount(AccountId),
    /// Přihlásit další účet.
    AddAccount,
    AskSignOut,
    CancelSignOut,
    /// Odhlásit aktivní účet (smaže přihlášení i jeho cache).
    SignOut,
    SignedOut(Result<(), Failure>),
    /// Server potvrdil (nebo odmítl) hlášení přečtení konverzace po zprávu (`u64` = její id).
    ReadReported(ConversationId, MessageId, Result<(), Failure>),
    /// Zapnout/vypnout spouštění po přihlášení do systému.
    ToggleAutostart,
    /// Zjistit nepřečtené u neaktivních účtů.
    PollOthers,
    OtherUnread(
        AccountId,
        Result<(OtherBackend, Vec<Conversation>), Failure>,
    ),
    /// Klik na notifikaci z neaktivního účtu: přepnout na něj a otevřít chat.
    OtherNotificationClicked(AccountId, ConversationId),
    /// Kontrola připomínek a obnova kalendářů.
    CalendarTick,
    CalendarLoaded(AccountId, Result<Vec<CalendarEvent>, Failure>),
    /// Uživatel reagoval na připomínku události.
    Reminder(AccountId, Box<CalendarEvent>, ReminderChoice),
    /// Otevřít událost v záložce Kalendář (id výskytu).
    SelectEvent(String),
    /// Otevřít chat schůzky.
    OpenMeetingChat(ConversationId),
    /// Připojit se ke schůzce z detailu (id výskytu, odkaz).
    JoinMeeting(String, String),
    /// Výsledek úlohy spuštěné v daném „období“ (při přepnutí účtu se staré zahodí).
    Scoped(u64, Box<Msg>),
    OpenUrl(String),
    /// Nová šířka okna (šířka bublin se odvíjí od místa ve vlákně).
    WindowResized(f32),
    Tick,
    /// Seznam konverzací; `bool` = všechny dosud načtené stránky (jinak jen nejnovější
    /// stránka ke sloučení).
    Conversations(Result<ConversationPage, Failure>, bool),
    /// Dočíst starší stránku seznamu konverzací (líné načítání).
    LoadOlderChats,
    /// Starší stránky seznamu konverzací; `usize` = kolik stránek jsme žádali.
    OlderChats(Result<ConversationPage, Failure>, usize),
    /// Posun seznamu chatů (na konci se dočtou starší).
    ChatListScrolled(scrollable::Viewport),
    PeersScanned(Vec<Message>),
    /// Jména účastníků z profilů: (chat, jména) a chaty, u kterých to nevyšlo.
    NamesResolved(Vec<(ConversationId, Vec<String>)>, Vec<ConversationId>),
    Select(ConversationId),
    /// `bool` = jde o starší stránku historie.
    Messages(ConversationId, Result<MessagePage, Failure>, bool),
    LoadOlder,
    /// Posun ve vláknu zpráv (sleduje, jestli je uživatel dole).
    Scrolled(scrollable::Viewport),
    /// Klik na „Nové zprávy“: posunout na nejnovější.
    JumpToNewest,
    Search(String),
    /// Uplynula pauza v psaní hledaného výrazu (`u64` = pořadí dotazu).
    SearchFire(u64),
    People(u64, Result<Vec<Person>, Failure>),
    External(u64, Result<Vec<Person>, Failure>),
    OpenPerson(Person),
    /// Nový skupinový chat: otevřít výběr lidí, název, hledání, přidání/odebrání, založení.
    NewGroup,
    GroupTopic(String),
    GroupQuery(String),
    GroupSearchFire(u64),
    GroupPeople(u64, Result<Vec<Person>, Failure>),
    GroupAdd(Person),
    GroupRemove(tymy_core::UserId),
    GroupCancel,
    GroupCreate,
    SetTab(Tab),
    /// Ctrl+V v psacím poli: přečíst schránku (obrázek nebo text).
    Paste,
    Pasted(Clip),
    /// Odebrat vložený obrázek před odesláním.
    RemovePending(usize),
    /// Backspace v prázdném psacím poli: odebrat poslední vložený obrázek.
    RemoveLastPending,
    /// Myš nad zprávou (ukáže tlačítka Kopírovat / Označit).
    HoverMessage(MessageId),
    UnhoverMessage(MessageId),
    /// Zkopírovat celý text zprávy.
    CopyMessage(MessageId),
    /// Zkopírovat text (např. přihlašovací kód) a ukázat potvrzení.
    CopyText(String, &'static str),
    /// Přepnout zprávu do režimu výběru textu.
    StartSelect(MessageId),
    StopSelect,
    /// Akce v textu zprávy v režimu výběru (úpravy se ignorují).
    SelectAction(text_editor::Action),
    /// Uplynula pauza po změně výběru → zkopírovat (`u64` = pořadí změny).
    CopySelection(u64),
    /// Skrýt oznámení (`u64` = pořadí oznámení).
    HideToast(u64),
    /// Přepnout vzhled (systém → světlý → tmavý).
    ToggleTheme,
    /// Systém hlásí světlý/tmavý režim.
    SystemTheme(iced::theme::Mode),
    /// Okno získalo / ztratilo fokus.
    WindowFocus(bool),
    /// Klik na notifikaci: otevřít chat a okno.
    NotificationClicked(ConversationId),
    /// Nic (např. zavřená notifikace).
    Noop,
    TrayReady(TrayHandle),
    TrayUnavailable,
    Tray(TrayEvent),
    /// Uživatel zavírá okno (s ikonou v liště se jen skryje).
    CloseRequested(iced::window::Id),
    /// Myš nad položkou seznamu (zobrazí ovládání připnutí).
    Hover(ConversationId),
    Unhover(ConversationId),
    Pin(ConversationId),
    Unpin(ConversationId),
    /// Posun připnutého chatu (záporně = nahoru).
    MovePin(ConversationId, isize),
    /// Úprava textu v psacím poli.
    Compose(text_editor::Action),
    /// Týdenní mřížka: posun o týdny (0 = tento týden), načtený týden, zavřít kartu události,
    /// Esc mimo psací pole.
    WeekShift(i64),
    /// Myš nad dnem mřížky (den, svislá pozice v px), stisk v prázdném místě dne,
    /// puštění tlačítka (kdekoli) → formulář nové schůzky s vybraným časem.
    GridMove(usize, f32),
    GridPress(usize),
    MouseUp,
    WeekLoaded(
        AccountId,
        chrono::NaiveDate,
        Result<Vec<CalendarEvent>, Failure>,
    ),
    CloseEvent,
    Escape,
    /// Nová schůzka: otevřít formulář, pole, Teams schůzka, hledání a výběr lidí, uložení.
    NewMeeting,
    MeetingInput(MeetingField, String),
    MeetingOnline(bool),
    MeetingSearchFire(u64),
    MeetingPeople(u64, Result<Vec<Person>, Failure>),
    MeetingAdd(Person),
    MeetingRemove(tymy_core::UserId),
    MeetingClose,
    MeetingCreate,
    MeetingCreated(AccountId, Result<String, Failure>),
    /// Zrušení vlastní schůzky (s potvrzením) a odpověď na pozvánku.
    AskCancelEvent(String),
    KeepEvent,
    CancelEvent(String),
    EventCancelled(String, Result<(), Failure>),
    ReplyEvent(String, EventReply),
    EventReplied(String, EventReply, Result<(), Failure>),
    /// Nastavení skupinového chatu: otevřít/zavřít, název, hledání lidí, přidání, odebrání,
    /// opuštění.
    ToggleChatPanel,
    PanelTopic(String),
    SaveTopic,
    TopicSaved(ConversationId, String, Result<(), Failure>),
    PanelQuery(String),
    PanelSearchFire(u64),
    PanelPeople(u64, Result<Vec<Person>, Failure>),
    AddMember(Person),
    MemberAdded(ConversationId, Person, Result<(), Failure>),
    RemoveMember(tymy_core::UserId),
    MemberRemoved(ConversationId, tymy_core::UserId, Result<(), Failure>),
    AskLeave,
    CancelLeave,
    Leave,
    Left(ConversationId, Result<(), Failure>),
    /// Otevřít/zavřít výběr emoji, přepnout jeho záložku, vložit emoji do psacího pole.
    ToggleEmoji,
    EmojiTab(Option<usize>),
    InsertEmoji(&'static str),
    /// Nabídka @zmínek: posun výběru, výběr (`None` = vybraná položka), zavření.
    MentionMove(isize),
    MentionPick(Option<usize>),
    MentionClose,
    /// Členové chatu pro nabídku zmínek.
    Roster(ConversationId, Result<Vec<Person>, Failure>),
    /// Odpovědět na zprávu (citace nad psacím polem).
    Reply(MessageId),
    CancelReply,
    /// Otevřít/zavřít výběr reakcí u zprávy.
    ToggleReactions(MessageId),
    /// Přidat nebo odebrat moji reakci (klíč Teams).
    React(MessageId, String),
    Reacted(ConversationId, Result<(), Failure>),
    /// Upravit vlastní zprávu (text do psacího pole).
    StartEdit(MessageId),
    /// Upravit poslední vlastní zprávu (↑ v prázdném poli).
    EditLast,
    CancelEdit,
    Edited(ConversationId, Result<(), Failure>),
    AskDelete(MessageId),
    CancelDelete,
    Delete(MessageId),
    Deleted(ConversationId, Result<(), Failure>),
    /// Otevřít zprávu nalezenou hledáním.
    OpenHit(ConversationId, MessageId),
    /// Klik na citaci odpovědi: přejít na původní zprávu.
    JumpToQuoted(MessageId),
    /// Další krok indexace historie na pozadí.
    IndexNext,
    Indexed(ConversationId, Result<MessagePage, Failure>),
    Send,
    /// Odesláno: (chat, text pro případ chyby, výsledek = skutečné id chatu).
    Sent(ConversationId, String, Result<ConversationId, Failure>),
    DismissError,
    MediaLoaded(String, Result<Arc<Vec<u8>>, Failure>),
    OpenImage(InlineImage),
    /// Stažený originál obrázku: (URL originálu, URL náhledu, výsledek).
    FullImage(String, String, Result<Arc<Vec<u8>>, Failure>),
}

impl Msg {
    /// Výsledky úloh, které se netýkají jen účtu otevřeného v okně (kalendáře a připomínky
    /// všech účtů): platí i po přepnutí účtu.
    fn global(&self) -> bool {
        matches!(self, Msg::CalendarLoaded(..) | Msg::Reminder(..))
    }

    /// Ladicí záznam o události: název a stručná metadata, nikdy obsah zpráv.
    fn log(&self) {
        use tracing::{debug, trace};
        let ok = |r: bool| if r { "ok" } else { "chyba" };
        match self {
            // časté a nezajímavé
            Msg::Scrolled(_) | Msg::ChatListScrolled(_) | Msg::Compose(_) | Msg::Search(_) => {
                trace!(event = ?std::mem::discriminant(self), "update")
            }
            Msg::Tick => trace!("update: Tick"),
            Msg::MentionMove(d) => trace!(d, "update: MentionMove"),
            Msg::ToggleEmoji => debug!("update: ToggleEmoji"),
            Msg::ToggleChatPanel => debug!("update: ToggleChatPanel"),
            Msg::NewMeeting => debug!("update: NewMeeting"),
            Msg::WeekShift(d) => debug!(d, "update: WeekShift"),
            Msg::GridMove(..) | Msg::MouseUp => trace!("update: GridMouse"),
            Msg::GridPress(day) => debug!(day, "update: GridPress"),
            Msg::WeekLoaded(account, week, r) => debug!(
                %account,
                %week,
                result = ok(r.is_ok()),
                count = r.as_ref().map(Vec::len).unwrap_or(0),
                "update: WeekLoaded"
            ),
            Msg::CloseEvent => debug!("update: CloseEvent"),
            Msg::Escape => trace!("update: Escape"),
            Msg::MeetingInput(..) => trace!("update: MeetingInput"),
            Msg::MeetingOnline(on) => debug!(on, "update: MeetingOnline"),
            Msg::MeetingSearchFire(seq) => debug!(seq, "update: MeetingSearchFire"),
            Msg::MeetingPeople(seq, r) => debug!(
                seq,
                result = ok(r.is_ok()),
                count = r.as_ref().map(Vec::len).unwrap_or(0),
                "update: MeetingPeople"
            ),
            Msg::MeetingAdd(p) => debug!(mri = %p.mri, "update: MeetingAdd"),
            Msg::MeetingRemove(mri) => debug!(%mri, "update: MeetingRemove"),
            Msg::MeetingClose => debug!("update: MeetingClose"),
            Msg::MeetingCreate => debug!("update: MeetingCreate"),
            Msg::MeetingCreated(account, r) => {
                debug!(%account, result = ok(r.is_ok()), "update: MeetingCreated")
            }
            Msg::AskCancelEvent(_) | Msg::KeepEvent => debug!("update: AskCancelEvent"),
            Msg::CancelEvent(_) => debug!("update: CancelEvent"),
            Msg::EventCancelled(_, r) => debug!(result = ok(r.is_ok()), "update: EventCancelled"),
            Msg::ReplyEvent(_, reply) => debug!(?reply, "update: ReplyEvent"),
            Msg::EventReplied(_, reply, r) => {
                debug!(?reply, result = ok(r.is_ok()), "update: EventReplied")
            }
            Msg::PanelTopic(_) | Msg::PanelQuery(_) => trace!("update: PanelInput"),
            Msg::SaveTopic => debug!("update: SaveTopic"),
            Msg::TopicSaved(id, _, r) => debug!(%id, result = ok(r.is_ok()), "update: TopicSaved"),
            Msg::PanelSearchFire(seq) => debug!(seq, "update: PanelSearchFire"),
            Msg::PanelPeople(seq, r) => debug!(
                seq,
                result = ok(r.is_ok()),
                count = r.as_ref().map(Vec::len).unwrap_or(0),
                "update: PanelPeople"
            ),
            Msg::AddMember(p) => debug!(mri = %p.mri, "update: AddMember"),
            Msg::MemberAdded(id, p, r) => {
                debug!(%id, mri = %p.mri, result = ok(r.is_ok()), "update: MemberAdded")
            }
            Msg::RemoveMember(mri) => debug!(%mri, "update: RemoveMember"),
            Msg::MemberRemoved(id, mri, r) => {
                debug!(%id, %mri, result = ok(r.is_ok()), "update: MemberRemoved")
            }
            Msg::AskLeave => debug!("update: AskLeave"),
            Msg::CancelLeave => debug!("update: CancelLeave"),
            Msg::Leave => debug!("update: Leave"),
            Msg::Left(id, r) => debug!(%id, result = ok(r.is_ok()), "update: Left"),
            Msg::EmojiTab(t) => trace!(?t, "update: EmojiTab"),
            Msg::InsertEmoji(_) => debug!("update: InsertEmoji"),
            Msg::MentionPick(i) => debug!(?i, "update: MentionPick"),
            Msg::MentionClose => debug!("update: MentionClose"),
            Msg::Roster(id, r) => debug!(
                %id,
                result = ok(r.is_ok()),
                count = r.as_ref().map(Vec::len).unwrap_or(0),
                "update: Roster"
            ),
            Msg::CalendarTick => trace!("update: CalendarTick"),
            Msg::CalendarLoaded(account, r) => debug!(
                %account,
                result = ok(r.is_ok()),
                count = r.as_ref().map(Vec::len).unwrap_or(0),
                "update: CalendarLoaded"
            ),
            Msg::Reminder(account, _, choice) => debug!(%account, ?choice, "update: Reminder"),
            Msg::SelectEvent(_) => debug!("update: SelectEvent"),
            Msg::OpenMeetingChat(id) => debug!(%id, "update: OpenMeetingChat"),
            Msg::JoinMeeting(..) => debug!("update: JoinMeeting"),
            Msg::Booted(r) => debug!(
                result = ok(r.is_ok()),
                account = matches!(r, Ok(Some(_))),
                "update: Booted"
            ),
            Msg::StartLogin(kind) => debug!(%kind, "update: StartLogin"),
            Msg::LoginCode(kind, r) => debug!(%kind, result = ok(r.is_ok()), "update: LoginCode"),
            Msg::LoginDone(r) => debug!(result = ok(r.is_ok()), "update: LoginDone"),
            Msg::CancelLogin => debug!("update: CancelLogin"),
            Msg::StartBrowserLogin(kind) => debug!(%kind, "update: StartBrowserLogin"),
            Msg::BrowserLoginInput(_) => debug!("update: BrowserLoginInput"),
            Msg::LoginClipboard(c) => debug!(some = c.is_some(), "update: LoginClipboard"),
            Msg::BrowserCode(r) => debug!(result = ok(r.is_ok()), "update: BrowserCode"),
            Msg::ToggleAccountMenu => debug!("update: ToggleAccountMenu"),
            Msg::SwitchAccount(id) => debug!(account = %id, "update: SwitchAccount"),
            Msg::AddAccount => debug!("update: AddAccount"),
            Msg::AskSignOut => debug!("update: AskSignOut"),
            Msg::CancelSignOut => debug!("update: CancelSignOut"),
            Msg::SignOut => debug!("update: SignOut"),
            Msg::SignedOut(r) => debug!(result = ok(r.is_ok()), "update: SignedOut"),
            Msg::PollOthers => trace!("update: PollOthers"),
            Msg::ToggleAutostart => debug!("update: ToggleAutostart"),
            Msg::ReadReported(c, id, r) => {
                debug!(conversation = %c, %id, result = ok(r.is_ok()), "update: ReadReported")
            }
            Msg::OtherUnread(id, r) => debug!(
                account = %id,
                result = ok(r.is_ok()),
                conversations = r.as_ref().map(|(_, c)| c.len()).unwrap_or(0),
                "update: OtherUnread"
            ),
            Msg::OtherNotificationClicked(a, c) => {
                debug!(account = %a, conversation = %c, "update: OtherNotificationClicked")
            }
            Msg::Scoped(epoch, inner) => {
                trace!(epoch, "update: Scoped");
                inner.log()
            }
            Msg::OpenUrl(_) => debug!("update: OpenUrl"),
            Msg::LoadOlderChats => debug!("update: LoadOlderChats"),
            Msg::OlderChats(r, pages) => debug!(
                result = ok(r.is_ok()),
                count = r.as_ref().map(|p| p.conversations.len()).unwrap_or(0),
                more = r.as_ref().map(|p| p.older.is_some()).unwrap_or(false),
                pages,
                "update: OlderChats"
            ),
            Msg::Conversations(r, full) => debug!(
                result = ok(r.is_ok()),
                count = r.as_ref().map(|p| p.conversations.len()).unwrap_or(0),
                full,
                "update: Conversations"
            ),
            Msg::PeersScanned(m) => debug!(messages = m.len(), "update: PeersScanned"),
            Msg::NamesResolved(ok, rest) => debug!(
                resolved = ok.len(),
                unresolved = rest.len(),
                "update: NamesResolved"
            ),
            Msg::Select(id) => debug!(%id, "update: Select"),
            Msg::Messages(id, r, older) => debug!(
                %id,
                older,
                result = ok(r.is_ok()),
                count = r.as_ref().map(|p| p.messages.len()).unwrap_or(0),
                has_older = r.as_ref().map(|p| p.older.is_some()).unwrap_or(false),
                "update: Messages"
            ),
            Msg::LoadOlder => debug!("update: LoadOlder"),
            Msg::JumpToNewest => debug!("update: JumpToNewest"),
            Msg::Send => debug!("update: Send"),
            Msg::Sent(id, _, r) => debug!(
                %id,
                result = ok(r.is_ok()),
                actual = ?r.as_ref().ok().filter(|a| *a != id).map(|a| a.as_str()),
                "update: Sent"
            ),
            Msg::DismissError => debug!("update: DismissError"),
            Msg::MediaLoaded(_, r) => debug!(
                result = ok(r.is_ok()),
                bytes = r.as_ref().map(|b| b.len()).unwrap_or(0),
                "update: MediaLoaded"
            ),
            Msg::OpenImage(_) => debug!("update: OpenImage"),
            Msg::SearchFire(seq) => debug!(seq, "update: SearchFire"),
            Msg::People(seq, r) => debug!(
                seq,
                result = ok(r.is_ok()),
                count = r.as_ref().map(Vec::len).unwrap_or(0),
                "update: People"
            ),
            Msg::External(seq, r) => debug!(
                seq,
                result = ok(r.is_ok()),
                count = r.as_ref().map(Vec::len).unwrap_or(0),
                "update: External"
            ),
            Msg::OpenPerson(p) => debug!(mri = %p.mri, external = p.external, "update: OpenPerson"),
            Msg::NewGroup => debug!("update: NewGroup"),
            Msg::GroupTopic(_) | Msg::GroupQuery(_) => trace!("update: GroupInput"),
            Msg::GroupSearchFire(seq) => debug!(seq, "update: GroupSearchFire"),
            Msg::GroupPeople(seq, r) => debug!(
                seq,
                result = ok(r.is_ok()),
                count = r.as_ref().map(Vec::len).unwrap_or(0),
                "update: GroupPeople"
            ),
            Msg::GroupAdd(p) => debug!(mri = %p.mri, "update: GroupAdd"),
            Msg::GroupRemove(mri) => debug!(%mri, "update: GroupRemove"),
            Msg::GroupCancel => debug!("update: GroupCancel"),
            Msg::GroupCreate => debug!("update: GroupCreate"),
            Msg::SetTab(tab) => debug!(?tab, "update: SetTab"),
            Msg::ToggleTheme => debug!("update: ToggleTheme"),
            Msg::HoverMessage(_) | Msg::UnhoverMessage(_) => trace!("update: HoverMessage"),
            Msg::Paste => debug!("update: Paste"),
            Msg::Pasted(c) => debug!(
                kind = match c {
                    Clip::Image { .. } => "image",
                    Clip::Text(_) => "text",
                    Clip::Empty => "empty",
                },
                "update: Pasted"
            ),
            Msg::RemovePending(i) => debug!(i, "update: RemovePending"),
            Msg::RemoveLastPending => debug!("update: RemoveLastPending"),
            Msg::CopyMessage(id) => debug!(%id, "update: CopyMessage"),
            Msg::CopyText(..) => debug!("update: CopyText"),
            Msg::StartSelect(id) => debug!(%id, "update: StartSelect"),
            Msg::StopSelect => debug!("update: StopSelect"),
            Msg::Reply(id) => debug!(%id, "update: Reply"),
            Msg::CancelReply => debug!("update: CancelReply"),
            Msg::ToggleReactions(id) => debug!(%id, "update: ToggleReactions"),
            Msg::React(id, key) => debug!(%id, %key, "update: React"),
            Msg::Reacted(id, r) => debug!(%id, result = ok(r.is_ok()), "update: Reacted"),
            Msg::StartEdit(id) => debug!(%id, "update: StartEdit"),
            Msg::EditLast => debug!("update: EditLast"),
            Msg::CancelEdit => debug!("update: CancelEdit"),
            Msg::Edited(id, r) => debug!(%id, result = ok(r.is_ok()), "update: Edited"),
            Msg::AskDelete(id) => debug!(%id, "update: AskDelete"),
            Msg::CancelDelete => debug!("update: CancelDelete"),
            Msg::Delete(id) => debug!(%id, "update: Delete"),
            Msg::Deleted(id, r) => debug!(%id, result = ok(r.is_ok()), "update: Deleted"),
            Msg::OpenHit(c, id) => debug!(conversation = %c, %id, "update: OpenHit"),
            Msg::JumpToQuoted(id) => debug!(%id, "update: JumpToQuoted"),
            Msg::IndexNext => trace!("update: IndexNext"),
            Msg::Indexed(id, r) => debug!(
                %id,
                result = ok(r.is_ok()),
                count = r.as_ref().map(|p| p.messages.len()).unwrap_or(0),
                "update: Indexed"
            ),
            Msg::SelectAction(_) => trace!("update: SelectAction"),
            Msg::CopySelection(seq) => debug!(seq, "update: CopySelection"),
            Msg::HideToast(_) => trace!("update: HideToast"),
            Msg::SystemTheme(mode) => debug!(?mode, "update: SystemTheme"),
            Msg::WindowFocus(f) => debug!(focused = f, "update: WindowFocus"),
            Msg::WindowResized(w) => trace!(width = w, "update: WindowResized"),
            Msg::NotificationClicked(id) => debug!(%id, "update: NotificationClicked"),
            Msg::Noop => trace!("update: Noop"),
            Msg::TrayReady(_) => debug!("update: TrayReady"),
            Msg::TrayUnavailable => debug!("update: TrayUnavailable"),
            Msg::Tray(e) => debug!(event = ?e, "update: Tray"),
            Msg::CloseRequested(_) => debug!("update: CloseRequested"),
            Msg::Hover(_) | Msg::Unhover(_) => trace!("update: Hover"),
            Msg::Pin(id) => debug!(%id, "update: Pin"),
            Msg::Unpin(id) => debug!(%id, "update: Unpin"),
            Msg::MovePin(id, delta) => debug!(%id, delta, "update: MovePin"),
            Msg::FullImage(_, _, r) => debug!(
                result = ok(r.is_ok()),
                bytes = r.as_ref().map(|b| b.len()).unwrap_or(0),
                "update: FullImage"
            ),
        }
    }
}

/// Stav staženého obrázku ze zprávy.
enum Media {
    Loading,
    Ready(image::Handle, Arc<Vec<u8>>),
    Failed,
}

/// Víc nových chatů najednou → jedna souhrnná notifikace.
const MAX_NOTIFICATIONS: usize = 3;

fn msg_num(id: &MessageId) -> u64 {
    id.as_str().parse().unwrap_or(0)
}

/// Hledání lidí: pauza v psaní před dotazem a minimální délka dotazu.
const SEARCH_DEBOUNCE: Duration = Duration::from_millis(300);
const MIN_PEOPLE_QUERY: usize = 2;

/// Za „dole“ ve vláknu považujeme i pozici do tolika pixelů od konce.
const BOTTOM_TOLERANCE: f32 = 40.0;

/// Horní mez velikosti obrázků v cache.
const MEDIA_CACHE_LIMIT: u64 = 500 * 1024 * 1024;

/// Kolik zpráv konverzace načíst z cache při otevření.
const CACHED_MESSAGES: usize = 300;
/// Stránka starších zpráv načítaná z cache.
const STORE_PAGE: usize = 100;
/// Kolik výsledků hledání ve zprávách zobrazit.
const MESSAGE_HITS: usize = 40;
/// Indexace historie: odklad po startu, pauza mezi požadavky a meze na konverzaci.
const INDEX_START_DELAY: Duration = Duration::from_secs(10);
const INDEX_DELAY: Duration = Duration::from_millis(1500);
const INDEX_HORIZON_DAYS: i64 = 365;
const INDEX_MAX_MESSAGES: i64 = 5000;

/// Pauza po označení textu, než se výběr zkopíruje (ať se nekopíruje při každém pohybu myši).
const COPY_DELAY: Duration = Duration::from_millis(350);
const TOAST_DURATION: Duration = Duration::from_millis(1800);

/// Šířka levého panelu.
const SIDEBAR_WIDTH: f32 = 320.0;
/// Týdenní mřížka: výška hodiny, sloupec s hodinami, okraje a místo pro posuvník.
const WEEK_HOUR: f32 = 44.0;
const WEEK_GUTTER: f32 = 44.0;
const WEEK_PAD: f32 = 12.0;
const WEEK_SCROLLBAR: f32 = 12.0;
const WEEK_GRID: Id = Id::new("week-grid");
/// Výchozí šířka okna (než přijde první událost o velikosti).
pub const WINDOW_WIDTH: f32 = 1100.0;

/// Maximální šířka obrázku v bublině.
const IMAGE_MAX_WIDTH: f32 = 360.0;

/// Obrázek ze schránky připravený k odeslání.
#[derive(Clone)]
pub struct PendingImage {
    png: Arc<Vec<u8>>,
    width: u32,
    height: u32,
    preview: image::Handle,
}

/// Rozepsaná zpráva chatu, který zrovna není otevřený: text, vložené obrázky a odpověď.
#[derive(Default)]
struct Draft {
    text: String,
    images: Vec<PendingImage>,
    reply: Option<ReplyRef>,
    mentions: Vec<MentionRef>,
}

impl Draft {
    fn is_empty(&self) -> bool {
        self.text.trim().is_empty() && self.images.is_empty() && self.reply.is_none()
    }
}

/// Zpráva v režimu výběru: její text v (jen pro čtení) editoru, který umí označování.
pub struct Selecting {
    id: MessageId,
    content: text_editor::Content,
    /// Šířka textu v bublině (editor potřebuje pevnou šířku).
    width: f32,
}

/// Záložka levého panelu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Chats,
    Teams,
    Calendar,
}

enum Phase {
    Starting,
    Login {
        code: Option<DeviceCode>,
        busy: bool,
        /// Přihlašuje se další účet: původní relace pro návrat tlačítkem Zpět.
        back: Option<Session>,
    },
    Ready(Session),
}

pub struct App {
    phase: Phase,
    /// Období úloh: zvýší se při přepnutí účtu, výsledky starších úloh se zahodí.
    epoch: u64,
    /// Přihlášené účty (z `accounts.toml`).
    accounts: Vec<AccountRecord>,
    account_menu: bool,
    confirm_sign_out: bool,
    /// Spouštět po přihlášení do systému (soubor v `~/.config/autostart`).
    autostart: bool,
    /// Neaktivní účty: spojení a nepřečtené; počet rozběhnutých dotazů.
    others: HashMap<AccountId, OtherAccount>,
    others_pending: usize,
    /// Běžící přihlašování (zruší se zahozením).
    login_task: Option<iced::task::Handle>,
    /// Chat k otevření po přepnutí účtu (klik na notifikaci z neaktivního účtu).
    open_after_switch: Option<ConversationId>,
    /// Typ účtu, který se právě přihlašuje.
    login_kind: AccountKind,
    /// Rozběhnuté přihlášení v prohlížeči.
    browser_login: Option<BrowserLogin>,
    /// Pole pro adresu z prohlížeče; probíhá výměna kódu za tokeny.
    login_input: String,
    redeeming: bool,
    conversations: Vec<Conversation>,
    peers: PeerNames,
    /// Zprávy podle konverzace, od nejstarší.
    messages: HashMap<ConversationId, Vec<Message>>,
    /// Kurzor na starší historii; `None` v mapě = ještě nenačteno, `Some(None)` = začátek.
    older: HashMap<ConversationId, Option<String>>,
    /// Co jsme přečetli v tomto okně.
    read_locally: HashMap<ConversationId, MessageId>,
    /// Kam až jsme přečtení nahlásili serveru (id zprávy v ms) a co se právě hlásí.
    reported_read: HashMap<ConversationId, u64>,
    reporting_read: HashSet<ConversationId>,
    selected: Option<ConversationId>,
    search: String,
    /// Psací pole (víceřádkové).
    composer: text_editor::Content,
    sending: bool,
    refreshing: bool,
    loading_older: bool,
    /// Je vlákno posunuté na konec (nejnovější zprávy)?
    at_bottom: bool,
    /// Přišla nová zpráva, zatímco uživatel četl starší (zobrazí overlay).
    new_below: bool,
    peer_scan_done: bool,
    /// Kdy jsme naposledy načítali celý seznam konverzací.
    last_full_refresh: Option<std::time::Instant>,
    /// Kurzor na další (starší) stránku seznamu konverzací; `None` = máme celý.
    chats_older: Option<String>,
    /// Kolik stránek seznamu konverzací máme načteno (tolik obnoví úplná obnova).
    chat_pages: usize,
    /// Právě se dočítají starší stránky seznamu.
    loading_chats: bool,
    /// Potřebujeme celý seznam (záložka Týmy, hledání): dočíst všechny stránky.
    want_all_chats: bool,
    /// Chaty, u kterých jsme už zkoušeli zjistit jména účastníků.
    names_tried: HashSet<ConversationId>,
    /// Konverzace, jejichž nejnovější stránku jsme v tomto běhu už stáhli ze sítě.
    fresh: std::collections::HashSet<ConversationId>,
    error: Option<String>,
    /// Pořadí posledního dotazu na hledání lidí (starší odpovědi se zahodí).
    search_seq: u64,
    /// Výsledky hledání lidí (adresář + externí).
    people: Vec<Person>,
    searching_people: bool,
    /// Poznámka k hledání (např. že externí hledání není k dispozici).
    people_note: Option<String>,
    /// Nové 1:1 chaty založené z hledání, které server zatím nezná (do první zprávy).
    draft_chats: Vec<Conversation>,
    /// S kým je rozpracovaný nový chat (pro založení vlákna při první zprávě).
    draft_people: HashMap<ConversationId, Person>,
    /// Rozpracované skupinové chaty (vzniknou první zprávou) a pořadí pro jejich dočasná id.
    draft_groups: HashMap<ConversationId, DraftGroup>,
    draft_seq: u64,
    /// Otevřený výběr lidí pro nový skupinový chat.
    group_builder: Option<GroupBuilder>,
    /// Otevřené nastavení skupinového chatu.
    chat_panel: Option<ChatPanel>,
    /// Zobrazený týden (pondělí) a načtené týdny aktivního účtu: události, kdy a co se načítá.
    week: chrono::NaiveDate,
    week_cache: HashMap<(AccountId, chrono::NaiveDate), Vec<CalendarEvent>>,
    week_fetched: HashMap<(AccountId, chrono::NaiveDate), DateTime<Utc>>,
    week_loading: HashSet<(AccountId, chrono::NaiveDate)>,
    /// Poslední pozice myši nad mřížkou (den, minuty) a rozpracovaný výběr času tažením.
    grid_hover: Option<(usize, i64)>,
    grid_drag: Option<GridDrag>,
    /// Formulář nové schůzky; událost čekající na potvrzení zrušení; běží změna události.
    meeting_form: Option<MeetingForm>,
    confirm_cancel_event: Option<String>,
    event_busy: bool,
    /// Obrázky ze zpráv podle URL.
    media: HashMap<String, Media>,
    tab: Tab,
    /// Obrázky vložené do psacího pole, čekající na odeslání.
    pending_images: Vec<PendingImage>,
    /// Rozepsané zprávy ostatních chatů (psací pole patří vždy otevřenému chatu).
    drafts: HashMap<ConversationId, Draft>,
    /// Rozepsaná @zmínka u kurzoru (nabídka lidí) a vybraná položka nabídky.
    mention: Option<mention::Query>,
    mention_index: usize,
    /// Zmínka zavřená Esc (znovu se nenabídne, dokud se nezmění).
    mention_dismissed: Option<(usize, usize)>,
    /// Lidé vybraní do zmínek v psacím poli; právě odesílané zmínky.
    composer_mentions: Vec<MentionRef>,
    in_flight_mentions: Vec<MentionRef>,
    /// Otevřený výběr emoji: záložka (`None` = naposledy použité) a naposledy použité.
    emoji_open: bool,
    emoji_tab: Option<usize>,
    recent_emoji: Vec<String>,
    /// Členové chatů pro nabídku zmínek (načtené při první „@“).
    rosters: HashMap<ConversationId, Vec<Person>>,
    roster_loading: HashSet<ConversationId>,
    /// Obrázky právě odesílané (vrátí se při chybě).
    in_flight_images: Vec<PendingImage>,
    /// Zpráva, na kterou se odpovídá, a odpověď právě odesílaná (vrátí se při chybě).
    replying: Option<ReplyRef>,
    in_flight_reply: Option<ReplyRef>,
    /// Upravovaná vlastní zpráva.
    editing: Option<Editing>,
    /// Zpráva s otevřeným výběrem reakcí / čekající na potvrzení smazání.
    reacting: Option<MessageId>,
    confirm_delete: Option<MessageId>,
    /// Zpráva otevřená z výsledků hledání (zvýrazněná).
    highlight: Option<MessageId>,
    /// Výsledky fulltextového hledání ve zprávách.
    message_hits: Vec<SearchHit>,
    /// Konverzace, které mají v cache starší zprávy, než je v paměti.
    older_in_store: HashSet<ConversationId>,
    /// Konverzace, ke kterým server odepřel přístup (403, uživatel už není členem).
    /// V seznamech se skryjí a znovu se nenačítají.
    forbidden: HashSet<ConversationId>,
    /// Indexace historie na pozadí: fronta, celkem, hotovo.
    index_queue: VecDeque<ConversationId>,
    index_total: usize,
    index_running: bool,
    /// Zpráva pod myší.
    hovered_message: Option<MessageId>,
    /// Zpráva v režimu výběru textu.
    selecting: Option<Selecting>,
    selection_seq: u64,
    /// Krátké oznámení nad vláknem a jeho pořadí.
    toast: Option<String>,
    toast_seq: u64,
    /// Zvolený vzhled a zda je systémový režim tmavý.
    theme_mode: ThemeMode,
    system_dark: bool,
    /// Má okno fokus (pro rozhodnutí o notifikacích)?
    focused: bool,
    /// Šířka okna (px); podle ní se počítá šířka bublin.
    window_width: f32,
    /// Notifikace až po prvním obnovení ze sítě.
    notify_ready: bool,
    tray: Option<TrayHandle>,
    /// Počet nepřečtených, který ikona v liště naposledy ukazovala.
    tray_unread: usize,
    /// Popisky schůzek, které ikona v liště naposledy ukazovala.
    tray_meetings: Vec<String>,
    /// Okno je skryté do lišty.
    hidden: bool,
    /// Ručně seřazené připnuté chaty (první = nahoře).
    pins: Vec<ConversationId>,
    hovered: Option<ConversationId>,
    /// Originály obrázků uložené na disk (URL originálu → soubor).
    originals: HashMap<String, std::path::PathBuf>,
    /// Kalendáře a připomínky (všechny účty).
    calendar: CalendarState,
}

impl App {
    pub fn boot() -> (Self, Task<Msg>) {
        let mut app = Self::blank();
        // `--hidden`: okno se nevytvoří viditelné (viz main), aplikace běží v liště
        app.hidden = std::env::args().any(|a| a == autostart::HIDDEN_ARG);
        let system_theme = iced::system::theme().map(Msg::SystemTheme);
        (
            app,
            Task::batch([Task::perform(load_session(None), Msg::Booted), system_theme]),
        )
    }

    /// Výchozí stav bez účtu.
    fn blank() -> Self {
        Self {
            phase: Phase::Starting,
            epoch: 0,
            accounts: load_accounts(),
            account_menu: false,
            confirm_sign_out: false,
            autostart: autostart::enabled(),
            others: HashMap::new(),
            others_pending: 0,
            login_task: None,
            open_after_switch: None,
            login_kind: AccountKind::Work,
            browser_login: None,
            login_input: String::new(),
            redeeming: false,
            conversations: vec![],
            peers: PeerNames::default(),
            messages: HashMap::new(),
            older: HashMap::new(),
            read_locally: HashMap::new(),
            reported_read: HashMap::new(),
            reporting_read: HashSet::new(),
            selected: None,
            search: String::new(),
            composer: text_editor::Content::new(),
            sending: false,
            refreshing: false,
            loading_older: false,
            at_bottom: true,
            new_below: false,
            peer_scan_done: false,
            last_full_refresh: None,
            chats_older: None,
            chat_pages: 1,
            loading_chats: false,
            want_all_chats: false,
            names_tried: HashSet::new(),
            fresh: Default::default(),
            error: None,
            media: HashMap::new(),
            tab: Tab::Chats,
            pending_images: vec![],
            drafts: HashMap::new(),
            emoji_open: false,
            emoji_tab: Some(0),
            recent_emoji: vec![],
            mention: None,
            mention_index: 0,
            mention_dismissed: None,
            composer_mentions: vec![],
            in_flight_mentions: vec![],
            rosters: HashMap::new(),
            roster_loading: HashSet::new(),
            in_flight_images: vec![],
            replying: None,
            in_flight_reply: None,
            editing: None,
            reacting: None,
            confirm_delete: None,
            highlight: None,
            message_hits: vec![],
            older_in_store: HashSet::new(),
            forbidden: HashSet::new(),
            index_queue: VecDeque::new(),
            index_total: 0,
            index_running: false,
            hovered_message: None,
            selecting: None,
            selection_seq: 0,
            toast: None,
            toast_seq: 0,
            theme_mode: Config::load().map(|c| c.settings.theme).unwrap_or_default(),
            system_dark: false,
            focused: true,
            window_width: WINDOW_WIDTH,
            notify_ready: false,
            tray: None,
            tray_unread: 0,
            tray_meetings: vec![],
            hidden: false,
            pins: vec![],
            hovered: None,
            search_seq: 0,
            people: vec![],
            searching_people: false,
            people_note: None,
            draft_chats: vec![],
            draft_people: HashMap::new(),
            draft_groups: HashMap::new(),
            draft_seq: 0,
            group_builder: None,
            chat_panel: None,
            week: cal::week_start(Local::now().date_naive()),
            week_cache: HashMap::new(),
            week_fetched: HashMap::new(),
            week_loading: HashSet::new(),
            grid_hover: None,
            grid_drag: None,
            meeting_form: None,
            confirm_cancel_event: None,
            event_busy: false,
            originals: HashMap::new(),
            calendar: CalendarState::load(),
        }
    }

    /// Zahodí stav aktivního účtu (zprávy, výběr, indexaci…) a zneplatní běžící úlohy.
    /// Vzhled, okno a ikona v liště zůstanou.
    fn reset_account_state(&mut self) {
        let mut fresh = Self::blank();
        fresh.epoch = self.epoch + 1;
        fresh.theme_mode = self.theme_mode;
        fresh.system_dark = self.system_dark;
        fresh.focused = self.focused;
        fresh.window_width = self.window_width;
        fresh.hidden = self.hidden;
        fresh.tray = self.tray.take();
        fresh.tray_unread = self.tray_unread;
        fresh.tray_meetings = std::mem::take(&mut self.tray_meetings);
        std::mem::swap(&mut fresh.calendar, &mut self.calendar);
        fresh.calendar.selected = None;
        *self = fresh;
    }

    /// Přepne na účet (nebo na naposledy použitý, je-li `None`).
    fn load_account(&mut self, id: Option<AccountId>) -> Task<Msg> {
        self.reset_account_state();
        Task::perform(load_session(id), Msg::Booted)
    }

    fn active_account(&self) -> Option<&AccountId> {
        match &self.phase {
            Phase::Ready(s) => Some(&s.profile.account_id),
            Phase::Login { back, .. } => back.as_ref().map(|s| &s.profile.account_id),
            Phase::Starting => None,
        }
    }

    /// Zahájí přihlášení (pro nový účet nebo po vypršení přihlášení).
    fn start_login(&mut self, kind: AccountKind) -> Task<Msg> {
        let back = match std::mem::replace(&mut self.phase, Phase::Starting) {
            Phase::Login { back, .. } => back,
            _ => None,
        };
        self.phase = Phase::Login {
            code: None,
            busy: true,
            back,
        };
        self.error = None;
        self.login_kind = kind;
        self.browser_login = None;
        let (task, handle) =
            Task::perform(start_login(kind), move |r| Msg::LoginCode(kind, r)).abortable();
        self.login_task = Some(handle.abort_on_drop());
        task
    }

    /// Přihlášení v prohlížeči: otevře přihlašovací stránku, u návratové adresy
    /// `http://localhost` čeká na kód místní server.
    fn start_browser_login(&mut self, kind: AccountKind) -> Task<Msg> {
        let ep = match Config::load() {
            Ok(c) => c.endpoints(kind, None),
            Err(e) => return self.fail(e.into()),
        };
        let login = match auth::start_browser_login(&ep) {
            Ok(l) => l,
            Err(e) => return self.fail(e.into()),
        };
        self.set_login(None, true);
        self.error = None;
        self.login_kind = kind;
        self.login_input.clear();
        self.redeeming = false;
        open_url(&login.url);
        let task = if login.uses_loopback() {
            let waiting = login.clone();
            let (task, handle) = Task::perform(
                async move { waiting.wait_loopback().await.map_err(Failure::from) },
                Msg::BrowserCode,
            )
            .abortable();
            self.login_task = Some(handle.abort_on_drop());
            task
        } else {
            self.login_task = None;
            Task::none()
        };
        self.browser_login = Some(login);
        task
    }

    /// Zkusí z textu (vložená adresa, schránka) získat kód a vyměnit ho za tokeny.
    fn accept_login_input(&mut self, input: &str, manual: bool) -> Task<Msg> {
        let Some(login) = self.browser_login.clone() else {
            return Task::none();
        };
        if self.redeeming {
            return Task::none();
        }
        match login.code_from(input, manual) {
            Ok(Some(code)) => self.redeem(login, code),
            Ok(None) => Task::none(),
            Err(e) => {
                tracing::warn!(error = %e, "přihlášení v prohlížeči");
                self.error = Some(e.to_string());
                Task::none()
            }
        }
    }

    fn redeem(&mut self, login: BrowserLogin, code: String) -> Task<Msg> {
        self.redeeming = true;
        self.error = None;
        let kind = self.login_kind;
        let (task, handle) =
            Task::perform(finish_browser_login(kind, login, code), Msg::LoginDone).abortable();
        self.login_task = Some(handle.abort_on_drop());
        task
    }

    fn set_login(&mut self, code: Option<DeviceCode>, busy: bool) {
        let back = match std::mem::replace(&mut self.phase, Phase::Starting) {
            Phase::Login { back, .. } => back,
            _ => None,
        };
        self.phase = Phase::Login { code, busy, back };
    }

    pub fn title(&self) -> String {
        let unread = self.conversations.iter().filter(|c| self.unread(c)).count();
        if unread > 0 {
            format!("({unread}) Tymy")
        } else {
            "Tymy".into()
        }
    }

    /// Téma podle volby (u „podle systému“ podle hlášeného režimu).
    pub fn theme(&self) -> Theme {
        let dark = match self.theme_mode {
            ThemeMode::Light => false,
            ThemeMode::Dark => true,
            ThemeMode::System => self.system_dark,
        };
        style::theme(dark)
    }

    pub fn subscription(&self) -> Subscription<Msg> {
        let focus = iced::event::listen_with(|event, _status, _id| match event {
            iced::Event::Window(iced::window::Event::Focused) => Some(Msg::WindowFocus(true)),
            iced::Event::Window(iced::window::Event::Unfocused) => Some(Msg::WindowFocus(false)),
            iced::Event::Window(iced::window::Event::Resized(size)) => {
                Some(Msg::WindowResized(size.width))
            }
            iced::Event::Window(iced::window::Event::Opened { size, .. }) => {
                Some(Msg::WindowResized(size.width))
            }
            iced::Event::Keyboard(iced::keyboard::Event::KeyPressed {
                key: iced::keyboard::Key::Named(iced::keyboard::key::Named::Escape),
                ..
            }) if _status == iced::event::Status::Ignored => Some(Msg::Escape),
            iced::Event::Mouse(iced::mouse::Event::ButtonReleased(iced::mouse::Button::Left)) => {
                Some(Msg::MouseUp)
            }
            _ => None,
        });
        let always = [
            focus,
            iced::system::theme_changes().map(Msg::SystemTheme),
            tray::subscription(),
            iced::window::close_requests().map(Msg::CloseRequested),
        ];
        match self.phase {
            Phase::Ready(_) => Subscription::batch(always.into_iter().chain([
                iced::time::every(REFRESH).map(|_| Msg::Tick),
                iced::time::every(CALENDAR_TICK).map(|_| Msg::CalendarTick),
                if self.accounts.len() > 1 {
                    iced::time::every(OTHERS_REFRESH).map(|_| Msg::PollOthers)
                } else {
                    Subscription::none()
                },
            ])),
            _ => Subscription::batch(always),
        }
    }

    /// Nejširší bublina zprávy podle místa ve vlákně: okno bez postranního panelu, odsazení
    /// vlákna (2 × 24 px), posuvníku, avataru a tlačítek u zprávy.
    fn bubble_max(&self) -> f32 {
        let pane = self.window_width - SIDEBAR_WIDTH - 48.0 - 12.0;
        let room = pane - MESSAGE_AVATAR - 6.0 * ICON_BUTTON - 8.0;
        room.clamp(BUBBLE_MIN, BUBBLE_MAX)
    }

    fn session(&self) -> Option<&Session> {
        match &self.phase {
            Phase::Ready(s) => Some(s),
            _ => None,
        }
    }

    fn fail(&mut self, f: Failure) -> Task<Msg> {
        tracing::error!(error = %f.text, reauth = f.reauth, "chyba");
        // při přihlašování (i dalšího účtu) zůstat na obrazovce přihlášení s návratem zpět
        if f.reauth && !matches!(self.phase, Phase::Login { .. }) {
            self.phase = Phase::Login {
                code: None,
                busy: false,
                back: None,
            };
        }
        self.error = Some(f.text);
        Task::none()
    }

    fn unread(&self, c: &Conversation) -> bool {
        let Some(s) = self.session() else {
            return false;
        };
        !self.forbidden.contains(&c.id) && is_unread(c, &s.profile, self.read_locally.get(&c.id))
    }

    fn fetch_messages(&self, id: ConversationId, cursor: Option<String>) -> Task<Msg> {
        let Some(s) = self.session() else {
            return Task::none();
        };
        if self.forbidden.contains(&id) || self.draft_groups.contains_key(&id) {
            return Task::none();
        }
        let backend = s.backend.clone();
        let older = cursor.is_some();
        Task::perform(
            {
                let id = id.clone();
                async move {
                    backend
                        .messages(&id, cursor.as_deref())
                        .await
                        .map_err(Failure::from)
                }
            },
            move |r| Msg::Messages(id.clone(), r, older),
        )
    }

    fn fetch_conversations(&mut self) -> Task<Msg> {
        let Some(backend) = self.session().map(|s| s.backend.clone()) else {
            return Task::none();
        };
        self.refreshing = true;
        let full = self
            .last_full_refresh
            .is_none_or(|t| t.elapsed() >= FULL_REFRESH);
        if full {
            self.last_full_refresh = Some(std::time::Instant::now());
        }
        let pages = if full { self.chat_pages } else { 1 };
        Task::perform(
            async move {
                backend
                    .conversation_pages(None, pages)
                    .await
                    .map_err(Failure::from)
            },
            move |r| Msg::Conversations(r, full),
        )
    }

    /// Dočte další starší stránku seznamu konverzací (při `want_all_chats` všechny zbylé).
    fn load_older_chats(&mut self) -> Task<Msg> {
        if self.loading_chats {
            return Task::none();
        }
        let (Some(cursor), Some(backend)) = (
            self.chats_older.clone(),
            self.session().map(|s| s.backend.clone()),
        ) else {
            return Task::none();
        };
        self.loading_chats = true;
        let pages = if self.want_all_chats {
            tymy_api::chat::MAX_CONVERSATION_PAGES
                .saturating_sub(self.chat_pages)
                .max(1)
        } else {
            1
        };
        Task::perform(
            async move {
                backend
                    .conversation_pages(Some(&cursor), pages)
                    .await
                    .map_err(Failure::from)
            },
            move |r| Msg::OlderChats(r, pages),
        )
    }

    /// Chceme celý seznam konverzací (Týmy, hledání): dočíst, co ještě chybí.
    fn want_all_chats(&mut self) -> Task<Msg> {
        self.want_all_chats = true;
        self.load_older_chats()
    }

    /// Je okno vidět a má fokus (uživatel opravdu čte otevřený chat)?
    fn visible(&self) -> bool {
        self.focused && !self.hidden
    }

    /// Označí otevřenou konverzaci jako přečtenou (jen když je okno vidět; serveru to
    /// pak nahlásí [`Self::report_read`]).
    fn mark_read(&mut self, id: &ConversationId) {
        if !self.visible() {
            return;
        }
        if let Some(last) = self.messages.get(id).and_then(|m| m.last()) {
            self.read_locally.insert(id.clone(), last.id.clone());
        }
        if let Some(last) = self
            .conversations
            .iter()
            .find(|c| &c.id == id)
            .and_then(|c| c.last_message.as_ref())
        {
            let newer = self.read_locally.get(id).is_none_or(|r| {
                r.as_str().parse::<u64>().unwrap_or(0) < last.id.as_str().parse().unwrap_or(0)
            });
            if newer {
                self.read_locally.insert(id.clone(), last.id.clone());
            }
        }
    }

    /// Přidá výsledky hledání (bez duplicit a beze mě).
    fn add_people(&mut self, found: Vec<Person>) {
        let me = self.session().map(|s| s.profile.account_id.clone());
        for p in found {
            let is_me = me
                .as_ref()
                .is_some_and(|m| p.mri.as_str().ends_with(m.as_str()));
            if !is_me && !self.people.iter().any(|q| q.mri == p.mri) {
                self.people.push(p);
            }
        }
    }

    /// Otevře 1:1 chat s osobou; pokud ještě neexistuje, připraví nový (vznikne první zprávou).
    fn open_person(&mut self, person: Person) -> Task<Msg> {
        let Some(me) = self.session().map(|s| s.profile.account_id.clone()) else {
            return Task::none();
        };
        let Some(other) = person.object_id() else {
            self.error = Some(format!(
                "Chat s {} zatím neumím založit (účet mimo Microsoft 365).",
                person.display_name
            ));
            return Task::none();
        };
        let id = one_on_one_id(me.as_str(), other);
        self.peers.set(&id, &person.display_name);
        if !self.conversations.iter().any(|c| c.id == id) {
            let draft = Conversation {
                id: id.clone(),
                kind: tymy_core::ConversationKind::OneOnOne,
                topic: None,
                last_message: None,
                last_activity: None,
                read_up_to: None,
                team: None,
            };
            self.draft_chats.push(draft);
            self.draft_people.insert(id.clone(), person.clone());
            self.merge_draft_chats();
        }
        self.search.clear();
        self.people.clear();
        self.people_note = None;
        self.search_seq += 1;
        self.update(Msg::Select(id))
    }

    /// Z výběru lidí udělá rozpracovaný skupinový chat a otevře ho; vlákno na serveru
    /// vznikne až s první zprávou (viz `Msg::Send`).
    fn create_group_draft(&mut self) -> Task<Msg> {
        let Some(g) = self.group_builder.take() else {
            return Task::none();
        };
        if g.members.len() < 2 {
            self.group_builder = Some(g);
            return Task::none();
        }
        self.draft_seq += 1;
        let id = ConversationId(format!("draft:group:{}", self.draft_seq));
        let topic = Some(g.topic.trim().to_owned()).filter(|t| !t.is_empty());
        let names: Vec<String> = g.members.iter().map(|p| p.display_name.clone()).collect();
        for n in names.iter().rev() {
            self.peers.set(&id, n);
        }
        self.draft_groups.insert(
            id.clone(),
            DraftGroup {
                members: g.members.iter().map(|p| p.mri.clone()).collect(),
                names,
                topic: topic.clone(),
            },
        );
        self.draft_chats.push(Conversation {
            id: id.clone(),
            kind: ConversationKind::Group,
            topic,
            last_message: None,
            last_activity: None,
            read_up_to: None,
            team: None,
        });
        self.merge_draft_chats();
        // prázdná historie, nic nenačítat ze serveru
        self.fresh.insert(id.clone());
        self.older.insert(id.clone(), None);
        self.messages.entry(id.clone()).or_default();
        self.tab = Tab::Chats;
        self.update(Msg::Select(id))
    }

    /// Zjistí jména účastníků chatů bez názvu z profilů: u 1:1 podle id chatu, u skupin
    /// podle členů vlákna. Každý chat zkouší jen jednou za běh.
    fn resolve_names(&mut self) -> Task<Msg> {
        let Some(s) = self.session() else {
            return Task::none();
        };
        let (backend, me) = (s.backend.clone(), s.profile.account_id.clone());
        let todo: Vec<(ConversationId, ConversationKind)> = self
            .conversations
            .iter()
            .filter(|c| {
                matches!(c.kind, ConversationKind::OneOnOne | ConversationKind::Group)
                    && c.topic.is_none()
                    && !self.peers.knows(&c.id)
                    && !self.names_tried.contains(&c.id)
                    && !self.is_draft(&c.id)
            })
            .map(|c| (c.id.clone(), c.kind.clone()))
            .collect();
        if todo.is_empty() {
            return Task::none();
        }
        self.names_tried
            .extend(todo.iter().map(|(id, _)| id.clone()));
        Task::perform(resolve_names(backend, me, todo), |(ok, rest)| {
            Msg::NamesResolved(ok, rest)
        })
    }

    /// Záloha: jména z historie zpráv (odesílatelé) u chatů, kde profily nevyšly.
    fn scan_peers(&self, ids: Vec<ConversationId>) -> Task<Msg> {
        let Some(backend) = self.session().map(|s| s.backend.clone()) else {
            return Task::none();
        };
        let ids: Vec<_> = ids.into_iter().take(PEER_SCAN_LIMIT).collect();
        if ids.is_empty() {
            return Task::none();
        }
        Task::perform(
            async move {
                let mut found = vec![];
                for id in ids {
                    match backend.messages(&id, None).await {
                        Ok(page) => found.extend(page.messages),
                        Err(e) => tracing::debug!(%id, error = %e, "sken jmen"),
                    }
                }
                found
            },
            Msg::PeersScanned,
        )
    }

    /// Notifikace pro nové zprávy v chatech od posledního obnovení. Při prvním obnovení ze sítě
    /// jen zapamatuje stav (žádná vlna starých zpráv po startu).
    fn notify_new(
        &mut self,
        session: &Session,
        previous: &HashMap<ConversationId, u64>,
    ) -> Task<Msg> {
        if !self.notify_ready {
            self.notify_ready = true;
            return Task::none();
        }
        let fresh: Vec<&Conversation> = self
            .conversations
            .iter()
            .filter(|c| c.is_chat())
            .filter(|c| {
                let Some(m) = &c.last_message else {
                    return false;
                };
                let newer = previous.get(&c.id).is_none_or(|p| msg_num(&m.id) > *p);
                let visible = self.focused && !self.hidden && self.selected.as_ref() == Some(&c.id);
                newer
                    && !visible
                    && m.is_user_content()
                    && !m.deleted
                    && !is_me(&session.profile, m)
            })
            .collect();
        if fresh.is_empty() {
            return Task::none();
        }
        if fresh.len() > MAX_NOTIFICATIONS {
            let n = fresh.len();
            return Task::perform(
                notify::show("Tymy".into(), format!("Nové zprávy v {n} chatech")),
                |_| Msg::Noop,
            );
        }
        Task::batch(fresh.into_iter().map(|c| {
            let m = c.last_message.as_ref().expect("filtrováno výše");
            let title = self.peers.title(c);
            let who = m.sender_name.clone().unwrap_or_default();
            let text = truncate(&preview_text(&m.content), 200);
            let body = if c.kind == ConversationKind::OneOnOne || who.is_empty() {
                text
            } else {
                format!("{who}: {text}")
            };
            let id = c.id.clone();
            Task::perform(notify::show(title, body), move |clicked| {
                if clicked {
                    Msg::NotificationClicked(id.clone())
                } else {
                    Msg::Noop
                }
            })
        }))
    }

    fn find_message(&self, id: &MessageId) -> Option<&Message> {
        let conv = self.selected.as_ref()?;
        self.messages.get(conv)?.iter().find(|m| &m.id == id)
    }

    /// Zapíše text do schránky i do primárního výběru (X11, vložení prostředním tlačítkem)
    /// a krátce ukáže oznámení.
    fn copy(&mut self, text: String, toast: &str) -> Task<Msg> {
        Task::batch([
            iced::clipboard::write(text.clone()),
            iced::clipboard::write_primary(text),
            self.show_toast(toast),
        ])
    }

    /// Nahlásí serveru přečtení otevřené konverzace, pokud jsme v okně přečetli víc,
    /// než server ví (a víc, než jsme už hlásili).
    fn report_read(&mut self) -> Task<Msg> {
        if !self.visible() {
            return Task::none();
        }
        let Some(conv) = self.selected.clone() else {
            return Task::none();
        };
        if self.is_draft(&conv)
            || self.forbidden.contains(&conv)
            || self.reporting_read.contains(&conv)
        {
            return Task::none();
        }
        let (Some(read), Some(backend)) = (
            self.read_locally.get(&conv).cloned(),
            self.session().map(|s| s.backend.clone()),
        ) else {
            return Task::none();
        };
        let n = msg_num(&read);
        let listed = self.conversations.iter().find(|c| c.id == conv);
        let on_server = listed
            .and_then(|c| c.read_up_to.as_ref())
            .map(msg_num)
            .unwrap_or(0);
        let reported = self.reported_read.get(&conv).copied().unwrap_or(0);
        if n == 0 || n <= on_server || n <= reported {
            return Task::none();
        }
        let client_id = self
            .messages
            .get(&conv)
            .and_then(|v| v.iter().rev().find(|m| m.id == read))
            .or_else(|| {
                listed
                    .and_then(|c| c.last_message.as_ref())
                    .filter(|m| m.id == read)
            })
            .and_then(|m| m.client_message_id.clone());
        self.reporting_read.insert(conv.clone());
        Task::perform(
            {
                let (conv, read) = (conv.clone(), read.clone());
                async move {
                    backend
                        .mark_read(&conv, &read, client_id.as_deref())
                        .await
                        .map_err(Failure::from)
                }
            },
            move |r| Msg::ReadReported(conv.clone(), read.clone(), r),
        )
    }

    /// Spustí zjištění nepřečtených u všech neaktivních účtů (pokud už neběží).
    fn poll_others(&mut self) -> Task<Msg> {
        if self.others_pending > 0 {
            return Task::none();
        }
        let Some(active) = self.session().map(|s| s.profile.account_id.clone()) else {
            return Task::none();
        };
        let tasks: Vec<_> = self
            .accounts
            .iter()
            .filter(|a| a.id != active)
            .map(|a| {
                let backend = self.others.get(&a.id).and_then(|o| o.backend.clone());
                let record = a.clone();
                let id = a.id.clone();
                Task::perform(other_unread(record, backend), move |r| {
                    Msg::OtherUnread(id.clone(), r)
                })
            })
            .collect();
        self.others_pending = tasks.len();
        Task::batch(tasks)
    }

    /// Účty, jejichž kalendář umíme načíst: aktivní a neaktivní s navázaným spojením.
    fn calendar_backends(&self) -> Vec<(AccountId, Arc<TeamsBackend>)> {
        let active = self
            .session()
            .map(|s| (s.profile.account_id.clone(), s.backend.clone()));
        let others = self.others.iter().filter_map(|(id, o)| {
            let b = o.backend.clone()?;
            (Some(id) != self.active_account()).then(|| (id.clone(), b))
        });
        active.into_iter().chain(others).collect()
    }

    /// Načte kalendáře účtů, které jsou starší než `max_age` a právě se nenačítají.
    fn refresh_calendar(&mut self, max_age: chrono::Duration) -> Task<Msg> {
        let now = Utc::now();
        let (from, to) = cal::fetch_range(now);
        let mut tasks = vec![];
        for (id, backend) in self.calendar_backends() {
            let fresh = self
                .calendar
                .fetched
                .get(&id)
                .is_some_and(|t| now - *t < max_age);
            if fresh || !self.calendar.loading.insert(id.clone()) {
                continue;
            }
            tasks.push(Task::perform(
                async move { backend.calendar(from, to).await.map_err(Failure::from) },
                move |r| Msg::CalendarLoaded(id.clone(), r),
            ));
        }
        Task::batch(tasks)
    }

    /// Zobrazí připomínky událostí, na které je čas (ze všech účtů). Účty, jejichž kalendář
    /// se právě načítá, počkají na čerstvá data (např. po probuzení ze spánku).
    fn check_reminders(&mut self) -> Task<Msg> {
        let now = Utc::now();
        // skončené (nebo mezitím zrušené či odmítnuté) schůzky už v liště neukazovat
        let current = &self.calendar.events;
        self.calendar.alerts.retain(|(account, e)| {
            let latest = current
                .get(account)
                .and_then(|evs| evs.iter().find(|x| x.id == e.id))
                .unwrap_or(e);
            latest.end > now && latest.wants_reminder()
        });
        if !self.calendar.settings.reminders {
            return Task::none();
        }
        let minutes = i64::from(self.calendar.settings.reminder_minutes);
        let known: HashSet<&AccountId> = self.accounts.iter().map(|a| &a.id).collect();
        let loading = &self.calendar.loading;
        let events = self
            .calendar
            .events
            .iter()
            .filter(|(id, _)| known.contains(id) && !loading.contains(*id))
            .flat_map(|(id, evs)| evs.iter().map(move |e| (id, e)));
        let due = self.calendar.reminders.take_due(events, now, minutes);
        for (account, e) in &due {
            let alerts = &mut self.calendar.alerts;
            alerts.retain(|(a, x)| !(a == account && x.id == e.id));
            alerts.push((account.clone(), e.clone()));
        }
        // připomenuté a neotevřené z dřívějška (po restartu aplikace)
        for (account, evs) in &self.calendar.events {
            if !known.contains(account) {
                continue;
            }
            for e in evs {
                let shown = self
                    .calendar
                    .alerts
                    .iter()
                    .any(|(a, x)| a == account && x.id == e.id);
                if !shown && e.end > now && self.calendar.reminders.is_pending(account, e) {
                    self.calendar.alerts.push((account.clone(), e.clone()));
                }
            }
        }
        let multi = self.accounts.len() > 1;
        let tasks: Vec<_> = due
            .into_iter()
            .map(|(account, e)| {
                tracing::info!(%account, "připomínka události");
                let mut body = cal::reminder_body(&e, now);
                if multi && let Some(a) = self.accounts.iter().find(|a| a.id == account) {
                    body.push_str(&format!("\nÚčet: {}", a.label()));
                }
                let can_join = e.join_url.is_some() && !e.cancelled;
                let title = e.subject.clone();
                let event = Box::new(e);
                Task::perform(notify::reminder(title, body, can_join), move |choice| {
                    Msg::Reminder(account.clone(), event.clone(), choice)
                })
            })
            .collect();
        Task::batch(tasks)
    }

    /// Uživatel si připomenutou schůzku otevřel: v liště ji už neukazovat.
    fn ack_meeting(&mut self, account: &AccountId, event: &str) {
        self.calendar
            .alerts
            .retain(|(a, e)| !(a == account && e.id == event));
        self.calendar.reminders.ack(account, event);
    }

    /// Popisky připomenutých schůzek pro lištu: „Porada v 10:00 (za 12 min)“.
    fn meeting_labels(&self) -> Vec<String> {
        let now = Utc::now();
        self.calendar
            .alerts
            .iter()
            .map(|(_, e)| format!("{} {}", e.subject, cal::when_label(e, now)))
            .collect()
    }

    /// Otevře událost v záložce Kalendář (u jiného účtu po přepnutí na něj).
    fn open_event(&mut self, account: AccountId, event: String) -> Task<Msg> {
        self.ack_meeting(&account, &event);
        if self.active_account() == Some(&account) {
            let tab = self.update(Msg::SetTab(Tab::Calendar));
            self.calendar.selected = Some(event);
            Task::batch([tab, self.show_window()])
        } else {
            tracing::info!(%account, "připomínka z jiného účtu: přepínám");
            let switch = self.load_account(Some(account));
            self.calendar.open_after_switch = Some(event);
            Task::batch([switch, self.show_window()])
        }
    }

    /// Upraví událost aktivního účtu v paměti (do další obnovy kalendáře).
    fn update_active_event(&mut self, id: &str, f: impl Fn(&mut CalendarEvent)) {
        let Some(account) = self.active_account().cloned() else {
            return;
        };
        let in_weeks = self
            .week_cache
            .iter_mut()
            .filter(|((a, _), _)| *a == account)
            .flat_map(|(_, evs)| evs.iter_mut());
        let in_list = self.calendar.events.get_mut(&account).into_iter().flatten();
        for e in in_list.chain(in_weeks).filter(|e| e.id == id) {
            f(e);
        }
    }

    /// Načte zobrazený týden aktivního účtu, pokud je starší než `max_age`.
    fn fetch_week(&mut self, max_age: chrono::Duration) -> Task<Msg> {
        let Some(s) = self.session() else {
            return Task::none();
        };
        let key = (s.profile.account_id.clone(), self.week);
        let fresh = self
            .week_fetched
            .get(&key)
            .is_some_and(|t| Utc::now() - *t < max_age);
        if fresh || self.week_loading.contains(&key) {
            return Task::none();
        }
        let backend = s.backend.clone();
        let (from, to) = cal::week_range(self.week);
        self.week_loading.insert(key.clone());
        Task::perform(
            async move { backend.calendar(from, to).await.map_err(Failure::from) },
            move |r| Msg::WeekLoaded(key.0.clone(), key.1, r),
        )
    }

    /// Posune mřížku na hodinu před `at`, jinak na začátek pracovního dne (u dneška hodinu
    /// před aktuálním časem).
    fn scroll_week(&self, at: Option<DateTime<Utc>>) -> Task<Msg> {
        let now = Utc::now();
        let hour = match at {
            Some(t) => t.with_timezone(&Local).hour() as f32 - 1.0,
            None if self.week == cal::week_start(now.with_timezone(&Local).date_naive()) => {
                (now.with_timezone(&Local).hour() as f32 - 1.0).min(8.0)
            }
            None => 7.0,
        };
        operation::scroll_to(
            WEEK_GRID,
            iced::widget::scrollable::AbsoluteOffset {
                x: None,
                y: Some(hour.max(0.0) * WEEK_HOUR),
            },
        )
    }

    /// Události zobrazeného týdne: načtené pro týden, jinak z kalendáře na 14 dní dopředu.
    fn week_events(&self) -> Vec<CalendarEvent> {
        let Some(account) = self.active_account() else {
            return vec![];
        };
        if let Some(evs) = self.week_cache.get(&(account.clone(), self.week)) {
            return evs.clone();
        }
        let (from, to) = cal::week_range(self.week);
        self.active_events()
            .unwrap_or_default()
            .iter()
            .filter(|e| e.start < to && e.end > from)
            .cloned()
            .collect()
    }

    /// Událost aktivního účtu podle id (z kalendáře na 14 dní nebo z načtených týdnů).
    fn find_event(&self, id: &str) -> Option<&CalendarEvent> {
        let account = self.active_account()?;
        let in_weeks = self
            .week_cache
            .iter()
            .filter(|((a, _), _)| a == account)
            .flat_map(|(_, evs)| evs.iter());
        self.active_events()
            .unwrap_or_default()
            .iter()
            .chain(in_weeks)
            .find(|e| e.id == id)
    }

    /// Události aktivního účtu (pokud je kalendář načtený).
    fn active_events(&self) -> Option<&[CalendarEvent]> {
        self.active_account()
            .and_then(|id| self.calendar.events.get(id))
            .map(Vec::as_slice)
    }

    /// Otevřená událost; bez výběru probíhající nebo nejbližší příští.
    fn selected_event(&self) -> Option<&CalendarEvent> {
        self.find_event(self.calendar.selected.as_deref()?)
    }

    /// Nepřečtené konverzace ve všech neaktivních účtech.
    fn others_unread(&self) -> usize {
        self.others.values().filter_map(|o| o.unread).sum()
    }

    /// Server odepřel přístup ke konverzaci: skrýt ji a dál nenačítat.
    fn mark_forbidden(&mut self, id: ConversationId, e: &Failure) {
        if self.forbidden.insert(id.clone()) {
            tracing::warn!(%id, error = %e.text, "konverzace bez přístupu, skrývám ji");
        }
        self.index_queue.retain(|c| c != &id);
    }

    /// Krátké oznámení nad vláknem.
    fn show_toast(&mut self, toast: &str) -> Task<Msg> {
        self.toast = Some(toast.to_owned());
        self.toast_seq += 1;
        let seq = self.toast_seq;
        Task::perform(tokio::time::sleep(TOAST_DURATION), move |_| {
            Msg::HideToast(seq)
        })
    }

    /// Klik na citaci odpovědi: přejít na původní zprávu v otevřeném chatu.
    fn jump_to_quoted(&mut self, id: MessageId) -> Task<Msg> {
        let Some(conv) = self.selected.clone() else {
            return Task::none();
        };
        let cached = self.store().is_some_and(|(store, account)| {
            store
                .load_messages_since(&account, &conv, msg_num(&id) as i64, 1)
                .ok()
                .and_then(|v| v.into_iter().next())
                .is_some_and(|m| m.id == id)
        });
        if cached {
            return self.open_hit(conv, id);
        }
        if self
            .messages
            .get(&conv)
            .is_some_and(|v| v.iter().any(|m| m.id == id))
        {
            // bez cache neumíme posunout přesně, aspoň zvýraznit
            self.highlight = Some(id);
            return Task::none();
        }
        self.show_toast("Původní zpráva není ve stažené historii")
    }

    fn save_pins(&self) {
        if let Some((store, account)) = self.store()
            && let Err(e) = store.save_pins(&account, &self.pins)
        {
            tracing::warn!(error = %e, "uložení připnutých chatů");
        }
    }

    /// Název pro zobrazení; u kanálu případně s týmem („Tým › kanál“).
    fn conv_title(&self, c: &Conversation, with_team: bool) -> String {
        if c.kind != ConversationKind::Channel {
            return self.peers.title(c);
        }
        let channel = channel_name(c);
        match self.team_name(c) {
            Some(team) if with_team => format!("{team} › {channel}"),
            _ => channel,
        }
    }

    fn team_name(&self, c: &Conversation) -> Option<String> {
        let team = c.team.as_ref()?;
        team.name.clone().or_else(|| {
            self.conversations
                .iter()
                .find(|x| x.id == team.id)
                .and_then(|x| x.team.as_ref()?.name.clone())
        })
    }

    fn is_draft(&self, id: &ConversationId) -> bool {
        self.draft_chats.iter().any(|d| &d.id == id)
    }

    /// Rozpracované nové chaty drží nahoře, dokud je server nezná.
    fn merge_draft_chats(&mut self) {
        let known: std::collections::HashSet<_> =
            self.conversations.iter().map(|c| c.id.clone()).collect();
        self.draft_chats.retain(|d| !known.contains(&d.id));
        for d in self.draft_chats.iter().rev() {
            self.conversations.insert(0, d.clone());
        }
    }

    /// Při startu: seznam chatů a jména účastníků z lokální cache (než odpoví síť).
    fn load_cached(&mut self, session: &Session) {
        let Some(store) = &session.store else { return };
        let account = &session.profile.account_id;
        match store.load_pins(account) {
            Ok(pins) => self.pins = pins,
            Err(e) => tracing::warn!(error = %e, "načtení připnutých chatů"),
        }
        match store.load_conversations(account) {
            Ok(list) => {
                self.conversations = list
                    .into_iter()
                    .filter(|c| c.kind != ConversationKind::System)
                    .collect();
                self.merge_draft_chats();
                self.peers.learn(
                    &session.profile,
                    self.conversations
                        .iter()
                        .filter_map(|c| c.last_message.as_ref()),
                );
                for c in &self.conversations {
                    if c.topic.is_none()
                        && !self.peers.knows(&c.id)
                        && let Ok(msgs) = store.load_messages(account, &c.id, 50)
                    {
                        self.peers.learn(&session.profile, msgs.iter());
                    }
                }
            }
            Err(e) => tracing::warn!(error = %e, "načtení cache"),
        }
    }

    /// Načte historii konverzace z cache, pokud ji ještě nemáme v paměti.
    fn load_cached_messages(&mut self, id: &ConversationId) -> Task<Msg> {
        if self.messages.contains_key(id) {
            return Task::none();
        }
        let Some((store, account)) = self
            .session()
            .and_then(|s| s.store.clone().map(|st| (st, s.profile.account_id.clone())))
        else {
            return Task::none();
        };
        let msgs = match store.load_messages(&account, id, CACHED_MESSAGES) {
            Ok(m) if !m.is_empty() => m,
            Ok(_) => return Task::none(),
            Err(e) => {
                tracing::warn!(error = %e, "načtení zpráv z cache");
                return Task::none();
            }
        };
        if let Ok(Some(cursor)) = store.older_cursor(&account, id) {
            self.older.insert(id.clone(), cursor);
        }
        if msgs.len() >= CACHED_MESSAGES {
            self.older_in_store.insert(id.clone());
        }
        let media = self.fetch_media_for(&msgs);
        self.messages.insert(id.clone(), msgs);
        media
    }

    /// Upraví zprávu v paměti i v cache; vrátí upravenou kopii (`None` = zpráva není načtená).
    fn change_message(
        &mut self,
        conv: &ConversationId,
        id: &MessageId,
        f: impl FnOnce(&mut Message),
    ) -> Option<Message> {
        let m = self
            .messages
            .get_mut(conv)?
            .iter_mut()
            .find(|m| &m.id == id)?;
        f(m);
        let copy = m.clone();
        if let Some((store, account)) = self.store()
            && let Err(e) = store.save_messages(&account, std::slice::from_ref(&copy))
        {
            tracing::warn!(error = %e, "uložení zprávy do cache");
        }
        Some(copy)
    }

    /// Ukončí úpravu zprávy a vrátí do psacího pole původně rozepsaný text.
    /// Odloží rozepsanou zprávu otevřeného chatu a do psacího pole dá koncept chatu `to`.
    fn switch_draft(&mut self, to: &ConversationId) {
        let current = Draft {
            text: self.composer.text(),
            images: std::mem::take(&mut self.pending_images),
            reply: self.replying.take(),
            mentions: std::mem::take(&mut self.composer_mentions),
        };
        self.mention = None;
        self.emoji_open = false;
        if let Some(from) = self.selected.clone() {
            if current.is_empty() {
                self.drafts.remove(&from);
            } else {
                self.drafts.insert(from, current);
            }
        }
        let next = self.drafts.remove(to).unwrap_or_default();
        self.composer = text_editor::Content::with_text(next.text.trim_end_matches('\n'));
        self.composer
            .perform(text_editor::Action::Move(text_editor::Motion::DocumentEnd));
        self.pending_images = next.images;
        self.replying = next.reply;
        self.composer_mentions = next.mentions;
    }

    /// Po změně psacího pole: rozepsaná @zmínka u kurzoru (a načtení členů chatu).
    fn refresh_mention(&mut self) -> Task<Msg> {
        let cursor = self.composer.cursor();
        let query = if self.editing.is_some() || cursor.selection.is_some() {
            None
        } else {
            let line = cursor.position.line;
            self.composer
                .line(line)
                .and_then(|l| mention::query_at(line, &l.text, cursor.position.column))
                .filter(|q| self.mention_dismissed != Some((q.line, q.at)))
                // za hotovou zmínkou („@Jan Novák …“) už nic nenabízet
                .filter(|q| {
                    !self
                        .composer_mentions
                        .iter()
                        .any(|m| q.text.starts_with(&m.name) && q.text.len() > m.name.len())
                })
        };
        let key = |q: &Option<mention::Query>| q.as_ref().map(|q| (q.text.clone(), q.at));
        if key(&query) != key(&self.mention) {
            self.mention_index = 0;
        }
        self.mention = query;
        if self.mention.is_none() {
            return Task::none();
        }
        self.fetch_roster()
    }

    /// Načte členy otevřeného chatu pro zmínky (jednou za běh).
    fn fetch_roster(&mut self) -> Task<Msg> {
        let (Some(id), Some(s)) = (self.selected.clone(), self.session()) else {
            return Task::none();
        };
        let kind = self
            .conversations
            .iter()
            .find(|c| c.id == id)
            .map(|c| c.kind.clone());
        // kanály mají celý tým; tam stačí odesílatelé zpráv
        let fits = matches!(
            kind,
            Some(ConversationKind::OneOnOne | ConversationKind::Group | ConversationKind::Meeting)
        );
        if !fits
            || self.is_draft(&id)
            || self.rosters.contains_key(&id)
            || self.roster_loading.contains(&id)
        {
            return Task::none();
        }
        let (backend, me) = (s.backend.clone(), my_mri(&s.profile));
        self.roster_loading.insert(id.clone());
        Task::perform(
            {
                let id = id.clone();
                async move {
                    let members = backend.members(&id).await?;
                    let others: Vec<_> = members.into_iter().filter(|m| *m != me).collect();
                    backend.profiles(&others).await
                }
            },
            move |r| Msg::Roster(id.clone(), r.map_err(Failure::from)),
        )
    }

    /// Lidé, které nabídnout k rozepsané zmínce: členové chatu a odesílatelé zpráv (bez mě).
    fn mention_candidates(&self) -> Vec<MentionRef> {
        const MAX: usize = 6;
        let (Some(q), Some(id), Some(s)) = (&self.mention, &self.selected, self.session()) else {
            return vec![];
        };
        let me = my_mri(&s.profile);
        let mut people: Vec<MentionRef> = vec![];
        let mut add = |mri: &tymy_core::UserId, name: &str| {
            let name = name.trim();
            if *mri != me && !name.is_empty() && !people.iter().any(|p| p.mri == *mri) {
                people.push(MentionRef {
                    mri: mri.clone(),
                    name: name.to_owned(),
                });
            }
        };
        // 1:1 chat: protějšek podle id chatu a jména v seznamu
        if let Some(oid) = me.as_str().strip_prefix("8:orgid:")
            && let Some(peer) = one_on_one_peer(id, oid)
            && let Some(c) = self.conversations.iter().find(|c| &c.id == id)
        {
            add(&peer, &self.peers.title(c));
        }
        for p in self.rosters.get(id).into_iter().flatten() {
            add(&p.mri, &p.display_name);
        }
        for m in self.messages.get(id).into_iter().flatten().rev() {
            if let (Some(mri), Some(name)) = (&m.sender, &m.sender_name) {
                add(mri, name);
            }
        }
        let mut found: Vec<MentionRef> = people
            .into_iter()
            .filter(|p| mention::matches(&p.name, &q.text))
            .collect();
        found.sort_by_key(|p| mention::fold(&p.name));
        found.truncate(MAX);
        found
    }

    /// Výběr emoji nad psacím polem: záložky kategorií a mřížka.
    fn emoji_panel(&self) -> Element<'_, Msg> {
        let tab = |label: &'static str, tip: &'static str, t: Option<usize>| {
            let active = self.emoji_tab == t;
            iced::widget::tooltip(
                button(
                    text(label)
                        .size(16)
                        .font(style::EMOJI)
                        .center()
                        .width(Length::Fill),
                )
                .width(36)
                .padding([3, 0])
                .style(move |th: &Theme, st| style::segment(th, st, active))
                .on_press(Msg::EmojiTab(t)),
                container(text(tip).size(11))
                    .padding([3, 8])
                    .style(style::tooltip),
                iced::widget::tooltip::Position::Top,
            )
        };
        let mut tabs = row![].spacing(2);
        if !self.recent_emoji.is_empty() {
            tabs = tabs.push(tab("🕘", "Naposledy použité", None));
        }
        for (i, (name, icon, _)) in emoji::CATEGORIES.iter().enumerate() {
            tabs = tabs.push(tab(icon, name, Some(i)));
        }
        let list: Vec<&'static str> = match self.emoji_tab {
            Some(i) => emoji::CATEGORIES
                .get(i)
                .map(|c| c.2.to_vec())
                .unwrap_or_default(),
            // naposledy použité: odkazy do tabulky (Msg nese &'static str)
            None => self
                .recent_emoji
                .iter()
                .filter_map(|r| {
                    emoji::CATEGORIES
                        .iter()
                        .flat_map(|c| c.2.iter())
                        .find(|e| **e == r.as_str())
                        .copied()
                })
                .collect(),
        };
        let rows = list.chunks(emoji::PER_ROW).map(|chunk| {
            row(chunk.iter().map(|e| {
                button(
                    text(*e)
                        .size(20)
                        .font(style::EMOJI)
                        .center()
                        .width(Length::Fill),
                )
                .width(36)
                .height(36)
                .padding(0)
                .style(style::ghost)
                .on_press(Msg::InsertEmoji(e))
                .into()
            }))
            .spacing(2)
            .into()
        });
        container(
            column![
                container(tabs).padding(3).style(style::segmented),
                scrollable(Column::with_children(rows).spacing(2))
                    .direction(style::scrollbar())
                    .style(style::thin_scrollbar)
                    .height(Length::Fixed(160.0)),
            ]
            .spacing(6),
        )
        .padding([6, 4])
        .max_width(400)
        .into()
    }

    /// Koncept chatu pro náhled v seznamu (první neprázdný řádek textu).
    fn draft_preview(&self, id: &ConversationId) -> Option<String> {
        let d = self.drafts.get(id).filter(|d| !d.is_empty())?;
        let line = d.text.lines().map(str::trim).find(|l| !l.is_empty());
        Some(match line {
            Some(l) => l.to_owned(),
            None if !d.images.is_empty() => "obrázek".into(),
            None => "odpověď".into(),
        })
    }

    fn cancel_edit(&mut self) {
        if let Some(ed) = self.editing.take() {
            self.composer = text_editor::Content::with_text(&ed.draft);
        }
    }

    /// Odešle úpravu vlastní zprávy (Enter v režimu úprav).
    fn submit_edit(&mut self, ed: Editing) -> Task<Msg> {
        let text = self.composer.text().trim().to_owned();
        if text.is_empty() {
            // prázdná úprava = nabídnout smazání
            self.confirm_delete = Some(ed.id.clone());
            return Task::none();
        }
        let Some(backend) = self.session().map(|s| s.backend.clone()) else {
            return Task::none();
        };
        self.cancel_edit();
        if rich::plain(&rich::parse(&ed.original)) == text {
            return Task::none(); // beze změny
        }
        let html = edited_html(&ed.original, &text);
        self.change_message(&ed.conversation, &ed.id, |m| {
            m.content = html.clone();
            m.edited_at = Some(Utc::now());
        });
        let (conv, id) = (ed.conversation, ed.id);
        Task::perform(
            {
                let conv = conv.clone();
                async move { backend.edit(&conv, &id, &html).await.map_err(Failure::from) }
            },
            move |r| Msg::Edited(conv.clone(), r),
        )
    }

    /// Otevře zprávu z výsledků hledání: načte z cache okolí zprávy až po nejnovější,
    /// odroluje nahoru a zprávu zvýrazní.
    fn open_hit(&mut self, conv: ConversationId, id: MessageId) -> Task<Msg> {
        let num = id.as_str().parse::<i64>().unwrap_or(0);
        if self.selected.as_ref() != Some(&conv) {
            self.replying = None;
            self.cancel_edit();
        }
        self.selected = Some(conv.clone());
        self.selecting = None;
        self.reacting = None;
        self.confirm_delete = None;
        let mut tasks = vec![];
        if let Some((store, account)) = self.store() {
            let mut window = store
                .load_messages_before(&account, &conv, num, 3)
                .unwrap_or_default();
            window.extend(
                store
                    .load_messages_since(&account, &conv, num, 2000)
                    .unwrap_or_default(),
            );
            let first = window.first().map(id_ms).unwrap_or(num);
            let entry = self.messages.entry(conv.clone()).or_default();
            entry.retain(|m| id_ms(m) >= first);
            merge_messages(entry, window.clone());
            self.older_in_store.insert(conv.clone());
            if !self.older.contains_key(&conv)
                && let Ok(Some(cursor)) = store.older_cursor(&account, &conv)
            {
                self.older.insert(conv.clone(), cursor);
            }
            tasks.push(self.fetch_media_for(&window));
        }
        self.highlight = Some(id);
        self.at_bottom = false;
        self.new_below = false;
        self.mark_read(&conv);
        if !self.fresh.contains(&conv) {
            tasks.push(self.fetch_messages(conv, None));
        }
        tasks.push(operation::snap_to(
            MESSAGES,
            scrollable::RelativeOffset::START,
        ));
        Task::batch(tasks)
    }

    /// Spustí indexaci historie všech konverzací na pozadí (jednou za běh).
    fn start_indexing(&mut self) -> Task<Msg> {
        if self.index_running || self.store().is_none() {
            return Task::none();
        }
        self.index_queue = self
            .conversations
            .iter()
            .filter(|c| !self.is_draft(&c.id) && !self.forbidden.contains(&c.id))
            .map(|c| c.id.clone())
            .collect();
        self.index_total = self.index_queue.len();
        self.index_running = true;
        tracing::info!(total = self.index_total, "indexace historie začne");
        Task::perform(tokio::time::sleep(INDEX_START_DELAY), |_| Msg::IndexNext)
    }

    /// Další stránka historie první nedokončené konverzace ve frontě.
    fn index_next(&mut self) -> Task<Msg> {
        let (Some((store, account)), Some(backend)) =
            (self.store(), self.session().map(|s| s.backend.clone()))
        else {
            self.index_running = false;
            return Task::none();
        };
        let horizon = (Utc::now() - chrono::Duration::days(INDEX_HORIZON_DAYS)).timestamp_millis();
        while let Some(conv) = self.index_queue.front().cloned() {
            let st = match store.history_state(&account, &conv) {
                Ok(st) => st,
                Err(e) => {
                    tracing::warn!(%conv, error = %e, "indexace: stav historie");
                    self.index_queue.pop_front();
                    continue;
                }
            };
            let done = matches!(st.cursor, Some(None))
                || st.count >= INDEX_MAX_MESSAGES
                || st.oldest.is_some_and(|o| o < horizon);
            if done {
                self.index_queue.pop_front();
                continue;
            }
            let cursor = st.cursor.flatten();
            return Task::perform(
                {
                    let conv = conv.clone();
                    async move {
                        backend
                            .history(&conv, cursor.as_deref())
                            .await
                            .map_err(Failure::from)
                    }
                },
                move |r| Msg::Indexed(conv.clone(), r),
            );
        }
        self.index_running = false;
        tracing::info!(total = self.index_total, "indexace historie hotová");
        Task::none()
    }

    fn store(&self) -> Option<(Store, AccountId)> {
        self.session()
            .and_then(|s| s.store.clone().map(|st| (st, s.profile.account_id.clone())))
    }

    /// Spustí stažení obrázků ze zpráv, které ještě nemáme.
    fn fetch_media_for(&mut self, messages: &[Message]) -> Task<Msg> {
        let Some(backend) = self.session().map(|s| s.backend.clone()) else {
            return Task::none();
        };
        let mut tasks = vec![];
        for m in messages.iter().filter(|m| !m.deleted) {
            for part in message_parts(&m.content) {
                let Part::Image(img) = part else { continue };
                if self.media.contains_key(&img.url) {
                    continue;
                }
                if let Some((store, account)) = self.store()
                    && let Ok(Some(bytes)) = store.load_media(&account, &img.url)
                {
                    let handle = image::Handle::from_bytes(bytes.clone());
                    self.media
                        .insert(img.url, Media::Ready(handle, Arc::new(bytes)));
                    continue;
                }
                self.media.insert(img.url.clone(), Media::Loading);
                let backend = backend.clone();
                let url = img.url;
                tasks.push(Task::perform(
                    {
                        let url = url.clone();
                        async move {
                            backend
                                .fetch_media(&url)
                                .await
                                .map(Arc::new)
                                .map_err(Failure::from)
                        }
                    },
                    move |r| Msg::MediaLoaded(url.clone(), r),
                ));
            }
        }
        Task::batch(tasks)
    }

    pub fn update(&mut self, msg: Msg) -> Task<Msg> {
        let msg = match msg {
            Msg::Scoped(epoch, inner) if epoch == self.epoch || inner.global() => *inner,
            Msg::Scoped(epoch, inner) => {
                tracing::debug!(
                    epoch,
                    current = self.epoch,
                    "zahazuji výsledek úlohy jiného účtu"
                );
                inner.log();
                return Task::none();
            }
            msg => msg,
        };
        let task = self.handle(msg);
        // výsledky úloh nesou období, ve kterém vznikly (viz `reset_account_state`)
        let epoch = self.epoch;
        let report = self.report_read();
        Task::batch([
            task.map(move |m| Msg::Scoped(epoch, Box::new(m))),
            report.map(move |m| Msg::Scoped(epoch, Box::new(m))),
            self.sync_tray(),
        ])
    }

    /// Aktualizuje počet nepřečtených v liště, pokud se změnil.
    fn sync_tray(&mut self) -> Task<Msg> {
        let Some(tray) = self.tray.clone() else {
            return Task::none();
        };
        // v liště součet za všechny účty
        let n = self.conversations.iter().filter(|c| self.unread(c)).count() + self.others_unread();
        let meetings = self.meeting_labels();
        if n == self.tray_unread && meetings == self.tray_meetings {
            return Task::none();
        }
        self.tray_unread = n;
        self.tray_meetings = meetings.clone();
        Task::perform(tray.set_state(n, meetings), |_| Msg::Noop)
    }

    /// Zobrazí okno (i skryté v liště) a dá mu fokus.
    fn show_window(&mut self) -> Task<Msg> {
        self.hidden = false;
        iced::window::latest().and_then(|id| {
            Task::batch([
                iced::window::set_mode(id, iced::window::Mode::Windowed),
                iced::window::gain_focus(id),
            ])
        })
    }

    fn handle(&mut self, msg: Msg) -> Task<Msg> {
        msg.log();
        match msg {
            Msg::Booted(Ok(Some(session))) => {
                self.accounts = load_accounts();
                tracing::info!(account = %session.profile.account_id, "účet načten");
                self.load_cached(&session);
                self.phase = Phase::Ready(session);
                // po přepnutí z připomínky jiného účtu otevřít její událost
                if let Some(event) = self.calendar.open_after_switch.take() {
                    self.tab = Tab::Calendar;
                    self.calendar.selected = Some(event);
                }
                // ladění: `TYMY_OPEN_TAB=calendar` otevře po startu záložku Kalendář
                // (`meeting` navíc otevře formulář nové schůzky, nic neukládá)
                let debug_tab = match std::env::var("TYMY_OPEN_TAB").as_deref() {
                    Ok("calendar") => Task::done(Msg::SetTab(Tab::Calendar)),
                    Ok("meeting") => {
                        Task::done(Msg::SetTab(Tab::Calendar)).chain(Task::done(Msg::NewMeeting))
                    }
                    _ => Task::none(),
                };
                Task::batch([
                    self.fetch_conversations(),
                    Task::done(Msg::CalendarTick),
                    debug_tab,
                    // nepřečtené ostatních účtů až po rozběhnutí aktivního
                    Task::perform(tokio::time::sleep(Duration::from_secs(3)), |_| {
                        Msg::PollOthers
                    }),
                ])
            }
            Msg::Booted(Ok(None)) => {
                self.accounts = load_accounts();
                self.phase = Phase::Login {
                    code: None,
                    busy: false,
                    back: None,
                };
                Task::none()
            }
            Msg::Booted(Err(e)) => {
                // bez relace zůstane přihlašovací obrazovka (s nabídkou jiných účtů)
                self.accounts = load_accounts();
                self.phase = Phase::Login {
                    code: None,
                    busy: false,
                    back: None,
                };
                self.fail(e)
            }

            Msg::StartLogin(kind) => self.start_login(kind),
            Msg::LoginCode(kind, Ok(code)) => {
                self.set_login(Some(code.clone()), true);
                let (task, handle) =
                    Task::perform(finish_login(kind, code), Msg::LoginDone).abortable();
                self.login_task = Some(handle.abort_on_drop());
                task
            }
            Msg::LoginCode(_, Err(e)) | Msg::LoginDone(Err(e)) | Msg::BrowserCode(Err(e)) => {
                self.login_task = None;
                self.browser_login = None;
                self.redeeming = false;
                self.set_login(None, false);
                self.fail(e)
            }
            Msg::StartBrowserLogin(kind) => self.start_browser_login(kind),
            Msg::BrowserLoginInput(text) => {
                self.login_input = text.clone();
                self.accept_login_input(&text, true)
            }
            Msg::LoginClipboard(Some(text)) => self.accept_login_input(&text, false),
            Msg::LoginClipboard(None) => Task::none(),
            Msg::BrowserCode(Ok(code)) => match self.browser_login.clone() {
                Some(login) if !self.redeeming => self.redeem(login, code),
                _ => Task::none(),
            },
            Msg::LoginDone(Ok(id)) => self.load_account(Some(id)),
            Msg::CancelLogin => {
                self.login_task = None;
                self.browser_login = None;
                self.redeeming = false;
                if let Phase::Login {
                    back: Some(session),
                    ..
                } = std::mem::replace(&mut self.phase, Phase::Starting)
                {
                    self.phase = Phase::Ready(session);
                }
                self.error = None;
                Task::none()
            }
            Msg::ToggleAccountMenu => {
                self.account_menu = !self.account_menu;
                self.confirm_sign_out = false;
                Task::none()
            }
            Msg::SwitchAccount(id) => {
                self.account_menu = false;
                if self.session().is_some_and(|s| s.profile.account_id == id) {
                    return Task::none();
                }
                tracing::info!(account = %id, "přepínám účet");
                self.load_account(Some(id))
            }
            Msg::AddAccount => {
                self.account_menu = false;
                if let Phase::Ready(session) = std::mem::replace(&mut self.phase, Phase::Starting) {
                    self.phase = Phase::Login {
                        code: None,
                        busy: false,
                        back: Some(session),
                    };
                }
                Task::none()
            }
            Msg::AskSignOut => {
                self.confirm_sign_out = true;
                Task::none()
            }
            Msg::CancelSignOut => {
                self.confirm_sign_out = false;
                Task::none()
            }
            Msg::SignOut => {
                let Some(id) = self.active_account().cloned() else {
                    return Task::none();
                };
                tracing::info!(account = %id, "odhlašuji účet");
                if let Some((store, account)) = self.store()
                    && let Err(e) = store.forget_account(&account)
                {
                    tracing::warn!(error = %e, "smazání cache účtu");
                }
                // nic dalšího pro tento účet už nespouštět
                self.reset_account_state();
                Task::perform(sign_out(id), Msg::SignedOut)
            }
            Msg::SignedOut(r) => {
                if let Err(e) = r {
                    self.error = Some(e.text.clone());
                    tracing::error!(error = %e.text, "odhlášení");
                }
                self.load_account(None)
            }
            Msg::Scoped(..) => Task::none(),
            Msg::PollOthers => self.poll_others(),
            Msg::OtherNotificationClicked(account, conv) => {
                tracing::info!(%account, "notifikace z jiného účtu: přepínám");
                let switch = self.load_account(Some(account));
                self.open_after_switch = Some(conv);
                Task::batch([switch, self.show_window()])
            }
            Msg::ToggleAutostart => {
                let on = !self.autostart;
                let result = std::env::current_exe()
                    .map_err(anyhow::Error::from)
                    .and_then(|exe| autostart::set(on, &exe));
                match result {
                    Ok(()) => {
                        tracing::info!(on, "spouštění po přihlášení");
                        self.autostart = on;
                        self.show_toast(if on {
                            "Tymy se spustí po přihlášení (v liště)"
                        } else {
                            "Spouštění po přihlášení vypnuto"
                        })
                    }
                    Err(e) => self.fail(e.into()),
                }
            }
            Msg::ReadReported(conv, id, r) => {
                self.reporting_read.remove(&conv);
                // i při chybě: znovu až s další zprávou, ať server nezahlcujeme
                self.reported_read.insert(conv.clone(), msg_num(&id));
                match r {
                    Ok(()) => {
                        if let Some(c) = self.conversations.iter_mut().find(|c| c.id == conv) {
                            c.read_up_to = Some(id);
                        }
                    }
                    Err(e) => {
                        tracing::warn!(%conv, error = %e.text, "hlášení přečtení selhalo");
                        if e.forbidden {
                            self.mark_forbidden(conv, &e);
                        }
                    }
                }
                Task::none()
            }
            Msg::OtherUnread(id, r) => {
                self.others_pending = self.others_pending.saturating_sub(1);
                let Some(record) = self.accounts.iter().find(|a| a.id == id).cloned() else {
                    return Task::none();
                };
                let entry = self.others.entry(id.clone()).or_default();
                match r {
                    Ok((backend, convs)) => {
                        let profile = record.profile();
                        entry.backend = Some(backend.0);
                        entry.unread = Some(unread_count(&convs, &profile));
                        entry.error = None;
                        let previous = entry.last_seen.replace(last_seen(&convs));
                        if let Some(previous) = previous {
                            return notify_other(&record, &profile, &convs, &previous);
                        }
                    }
                    Err(e) => {
                        tracing::warn!(account = %id, error = %e.text, "nepřečtené neaktivního účtu");
                        entry.unread = None;
                        entry.error = Some(if e.reauth {
                            "přihlášení vypršelo".into()
                        } else {
                            "nedostupné".into()
                        });
                    }
                }
                Task::none()
            }

            Msg::OpenUrl(url) => {
                open_url(&url);
                Task::none()
            }

            Msg::CalendarTick => {
                let refresh =
                    self.refresh_calendar(chrono::Duration::minutes(CALENDAR_REFRESH_MINUTES));
                let week = if self.tab == Tab::Calendar {
                    self.fetch_week(chrono::Duration::minutes(CALENDAR_REFRESH_MINUTES))
                } else {
                    Task::none()
                };
                Task::batch([refresh, week, self.check_reminders()])
            }
            Msg::CalendarLoaded(id, r) => {
                self.calendar.loading.remove(&id);
                self.calendar.fetched.insert(id.clone(), Utc::now());
                match r {
                    Ok(events) => {
                        self.calendar.errors.remove(&id);
                        self.calendar.events.insert(id, events);
                    }
                    Err(e) => {
                        tracing::warn!(account = %id, error = %e.text, "kalendář nelze načíst");
                        self.calendar.errors.insert(id, e.text);
                    }
                }
                self.check_reminders()
            }
            Msg::Reminder(account, event, choice) => match choice {
                ReminderChoice::Open => self.open_event(account, event.id),
                ReminderChoice::Join => {
                    self.ack_meeting(&account, &event.id);
                    if let Some(url) = &event.join_url {
                        open_url(url);
                    }
                    Task::none()
                }
                ReminderChoice::Snooze => {
                    self.calendar.reminders.snooze(&account, &event, Utc::now());
                    Task::none()
                }
                ReminderChoice::Dismissed => Task::none(),
            },
            Msg::SelectEvent(id) => {
                self.meeting_form = None;
                self.confirm_cancel_event = None;
                if let Some(account) = self.active_account().cloned() {
                    self.ack_meeting(&account, &id);
                }
                let start = self.find_event(&id).map(|e| (cal::start_day(e), e.start));
                self.calendar.selected = Some(id);
                match start {
                    Some((day, at)) => {
                        self.week = cal::week_start(day);
                        let fetch = self.fetch_week(chrono::Duration::minutes(1));
                        Task::batch([fetch, self.scroll_week(Some(at))])
                    }
                    None => Task::none(),
                }
            }
            Msg::JoinMeeting(id, url) => {
                if let Some(account) = self.active_account().cloned() {
                    self.ack_meeting(&account, &id);
                }
                open_url(&url);
                Task::none()
            }
            Msg::OpenMeetingChat(id) => {
                let tab = self.update(Msg::SetTab(Tab::Chats));
                Task::batch([tab, self.update(Msg::Select(id))])
            }

            Msg::Tick => {
                if self.refreshing {
                    Task::none()
                } else {
                    self.fetch_conversations()
                }
            }
            Msg::Conversations(Ok(page), full) => {
                self.refreshing = false;
                let Some(session) = self.session().cloned() else {
                    return Task::none();
                };
                let list = page.conversations;
                let mut more = None;
                let list = if full {
                    // Za načtenými stránkami jsou ještě starší: ty, které známe (z cache nebo
                    // z dřívějška) a jsou starší než všechno načtené, zatím ponecháme.
                    self.chats_older = page
                        .older
                        .filter(|_| self.chat_pages < tymy_api::chat::MAX_CONVERSATION_PAGES);
                    more = Some(self.chats_older.is_some() && self.want_all_chats);
                    match list.last().map(|c| c.last_activity) {
                        Some(oldest) if self.chats_older.is_some() => {
                            let fresh: HashSet<ConversationId> =
                                list.iter().map(|c| c.id.clone()).collect();
                            let beyond = self
                                .conversations
                                .iter()
                                .filter(|c| {
                                    !fresh.contains(&c.id)
                                        && !self.is_draft(&c.id)
                                        && c.last_activity < oldest
                                })
                                .cloned()
                                .collect::<Vec<_>>();
                            list.into_iter().chain(beyond).collect()
                        }
                        _ => list,
                    }
                } else {
                    // nejnovější stránka: starší konverzace zůstanou z posledního celého seznamu
                    let fresh: HashSet<ConversationId> =
                        list.iter().map(|c| c.id.clone()).collect();
                    let older = self
                        .conversations
                        .iter()
                        .filter(|c| !fresh.contains(&c.id) && !self.is_draft(&c.id))
                        .cloned();
                    list.into_iter().chain(older).collect::<Vec<_>>()
                };
                if let Some(store) = &session.store
                    && let Err(e) = store.save_conversations(&session.profile.account_id, &list)
                {
                    tracing::warn!(error = %e, "uložení konverzací do cache");
                }
                let previous: HashMap<ConversationId, u64> = self
                    .conversations
                    .iter()
                    .filter_map(|c| Some((c.id.clone(), msg_num(&c.last_message.as_ref()?.id))))
                    .collect();
                self.conversations = list
                    .into_iter()
                    .filter(|c| c.kind != ConversationKind::System)
                    .collect();
                self.merge_draft_chats();
                self.peers.learn(
                    &session.profile,
                    self.conversations
                        .iter()
                        .filter_map(|c| c.last_message.as_ref()),
                );
                let mut tasks = vec![self.notify_new(&session, &previous)];
                // Nová zpráva v otevřeném chatu → načíst ji.
                if let Some(sel) = self.selected.clone() {
                    let server_last = self
                        .conversations
                        .iter()
                        .find(|c| c.id == sel)
                        .and_then(|c| c.last_message.as_ref())
                        .map(|m| m.id.clone());
                    let local_last = self
                        .messages
                        .get(&sel)
                        .and_then(|m| m.last())
                        .map(|m| m.id.clone());
                    if server_last.is_some() && server_last != local_last {
                        tasks.push(self.fetch_messages(sel.clone(), None));
                    }
                    self.mark_read(&sel);
                }
                if !self.peer_scan_done {
                    self.peer_scan_done = true;
                    tasks.push(self.start_indexing());
                    // ladění: `TYMY_OPEN_CHAT=<začátek id nebo část názvu>` otevře chat po startu
                    if let Ok(want) = std::env::var("TYMY_OPEN_CHAT")
                        && !want.is_empty()
                    {
                        let want = want.to_lowercase();
                        if let Some(c) = self.conversations.iter().find(|c| {
                            c.id.as_str().to_lowercase().starts_with(&want)
                                || self.peers.title(c).to_lowercase().contains(&want)
                        }) {
                            tasks.push(Task::done(Msg::Select(c.id.clone())));
                            // ladění: `TYMY_COMPOSE=<text>` vloží text do psacího pole
                            // (nic neodesílá; např. zkouška nabídky zmínek)
                            // ladění: `TYMY_CHAT_PANEL=1` otevře nastavení skupinového chatu
                            if std::env::var_os("TYMY_CHAT_PANEL").is_some() {
                                tasks.push(Task::done(Msg::ToggleChatPanel));
                            }
                            // ladění: `TYMY_EMOJI=1` otevře výběr emoji
                            if std::env::var_os("TYMY_EMOJI").is_some() {
                                tasks.push(Task::done(Msg::ToggleEmoji));
                            }
                            if let Ok(t) = std::env::var("TYMY_COMPOSE") {
                                tasks.push(Task::done(Msg::Compose(text_editor::Action::Edit(
                                    text_editor::Edit::Paste(Arc::new(t)),
                                ))));
                            }
                        }
                    }
                    // ladění: `TYMY_SEARCH=<dotaz>` vyplní hledání
                    if let Ok(q) = std::env::var("TYMY_SEARCH") {
                        tasks.push(Task::done(Msg::Search(q)));
                    }
                }
                // jména účastníků chatů bez názvu (nové chaty i za běhu)
                tasks.push(self.resolve_names());
                // po přepnutí z notifikace jiného účtu otevřít její chat
                if let Some(conv) = self.open_after_switch.take() {
                    self.tab = Tab::Chats;
                    tasks.push(Task::done(Msg::Select(conv)));
                }
                if more == Some(true) {
                    tasks.push(self.load_older_chats());
                }
                Task::batch(tasks)
            }
            Msg::LoadOlderChats => self.load_older_chats(),
            Msg::ChatListScrolled(viewport) => {
                let rel = viewport.relative_offset().y;
                if self.tab != Tab::Calendar && rel.is_finite() && rel >= 0.9 {
                    self.load_older_chats()
                } else {
                    Task::none()
                }
            }
            Msg::OlderChats(Ok(page), pages) => {
                self.loading_chats = false;
                let Some(session) = self.session().cloned() else {
                    return Task::none();
                };
                self.chat_pages =
                    (self.chat_pages + pages).min(tymy_api::chat::MAX_CONVERSATION_PAGES);
                // na limitu stránek už dál nedočítáme
                self.chats_older = page
                    .older
                    .filter(|_| self.chat_pages < tymy_api::chat::MAX_CONVERSATION_PAGES);
                let older: HashMap<ConversationId, Conversation> = page
                    .conversations
                    .into_iter()
                    .filter(|c| c.kind != ConversationKind::System)
                    .map(|c| (c.id.clone(), c))
                    .collect();
                // Známý chat: novější stav (z obnovy první stránky) má přednost, starší
                // (z cache) nahradíme tím ze serveru.
                let mut older = older;
                for c in &mut self.conversations {
                    if let Some(o) = older.remove(&c.id)
                        && o.last_activity >= c.last_activity
                    {
                        *c = o;
                    }
                }
                let added = older.len();
                self.conversations.extend(older.into_values());
                self.conversations
                    .sort_by_key(|c| std::cmp::Reverse(c.last_activity));
                tracing::info!(
                    added,
                    total = self.conversations.len(),
                    more = self.chats_older.is_some(),
                    "dočteny starší konverzace"
                );
                let real: Vec<Conversation> = self
                    .conversations
                    .iter()
                    .filter(|c| !self.is_draft(&c.id))
                    .cloned()
                    .collect();
                if let Some(store) = &session.store
                    && let Err(e) = store.save_conversations(&session.profile.account_id, &real)
                {
                    tracing::warn!(error = %e, "uložení konverzací do cache");
                }
                self.peers.learn(
                    &session.profile,
                    self.conversations
                        .iter()
                        .filter_map(|c| c.last_message.as_ref()),
                );
                let mut tasks = vec![self.resolve_names()];
                if self.want_all_chats {
                    tasks.push(self.load_older_chats());
                }
                Task::batch(tasks)
            }
            Msg::OlderChats(Err(e), _) => {
                self.loading_chats = false;
                self.fail(e)
            }
            Msg::Conversations(Err(e), full) => {
                if full {
                    // celý seznam zkusit znovu při další obnově
                    self.last_full_refresh = None;
                }
                self.refreshing = false;
                self.fail(e)
            }
            Msg::NamesResolved(resolved, unresolved) => {
                for (id, names) in resolved {
                    self.peers.replace(&id, names);
                }
                self.scan_peers(unresolved)
            }
            Msg::PeersScanned(found) => {
                if let Some(s) = self.session().cloned() {
                    self.peers.learn(&s.profile, found.iter().rev());
                    if let Some(store) = &s.store
                        && let Err(e) = store.save_messages(&s.profile.account_id, &found)
                    {
                        tracing::warn!(error = %e, "uložení zpráv do cache");
                    }
                }
                Task::none()
            }

            Msg::Select(id) => {
                self.group_builder = None;
                if self.chat_panel.as_ref().is_some_and(|p| p.conv != id) {
                    self.chat_panel = None;
                }
                if self.selected.as_ref() != Some(&id) {
                    self.cancel_edit();
                    self.switch_draft(&id);
                }
                self.highlight = None;
                self.reacting = None;
                self.confirm_delete = None;
                self.selected = Some(id.clone());
                self.selecting = None;
                self.at_bottom = true;
                self.new_below = false;
                let cached = self.load_cached_messages(&id);
                self.mark_read(&id);
                let fetch = if self.fresh.contains(&id) {
                    Task::none()
                } else {
                    self.fetch_messages(id, None)
                };
                Task::batch([
                    cached,
                    fetch,
                    operation::focus(COMPOSER),
                    operation::snap_to_end(MESSAGES),
                ])
            }
            Msg::Messages(id, Ok(page), older) => {
                if older {
                    self.loading_older = false;
                }
                let session = self.session().cloned();
                if let Some(s) = &session {
                    self.peers.learn(&s.profile, page.messages.iter().rev());
                }
                if !older {
                    self.fresh.insert(id.clone());
                }
                let first_load = !self.older.contains_key(&id);
                let store = session
                    .as_ref()
                    .and_then(|s| s.store.as_ref().map(|st| (st, &s.profile.account_id)));
                if let Some((store, account)) = store
                    && let Err(e) = store.save_messages(account, &page.messages)
                {
                    tracing::warn!(error = %e, "uložení zpráv do cache");
                }
                if older || first_load {
                    if let Some((store, account)) = store
                        && let Err(e) = store.set_older_cursor(account, &id, page.older.as_deref())
                    {
                        tracing::warn!(error = %e, "uložení kurzoru do cache");
                    }
                    self.older.insert(id.clone(), page.older);
                }
                let media = self.fetch_media_for(&page.messages);
                let newest = |msgs: Option<&Vec<Message>>| {
                    msgs.and_then(|m| m.iter().rev().find(|m| displayable(m)))
                        .map(|m| m.id.as_str().parse::<u64>().unwrap_or(0))
                };
                let before = newest(self.messages.get(&id));
                merge_messages(self.messages.entry(id.clone()).or_default(), page.messages);
                let after = newest(self.messages.get(&id));
                let mut scroll = Task::none();
                if self.selected.as_ref() == Some(&id) {
                    self.mark_read(&id);
                    if !older && after > before {
                        if before.is_none() || self.at_bottom {
                            scroll = operation::snap_to_end(MESSAGES);
                        } else {
                            self.new_below = true;
                        }
                    }
                }
                Task::batch([media, scroll])
            }
            Msg::Messages(id, Err(e), older) => {
                if older {
                    self.loading_older = false;
                }
                // Nový chat z hledání: vlákno na serveru ještě není → prázdná historie.
                if e.not_found && self.is_draft(&id) {
                    tracing::debug!(%id, "nový chat zatím bez vlákna na serveru");
                    self.fresh.insert(id.clone());
                    self.older.insert(id.clone(), None);
                    self.messages.entry(id).or_default();
                    return Task::none();
                }
                if e.forbidden {
                    self.mark_forbidden(id, &e);
                    return Task::none();
                }
                self.fail(e)
            }
            Msg::LoadOlder => {
                let Some(id) = self.selected.clone() else {
                    return Task::none();
                };
                if self.loading_older {
                    return Task::none();
                }
                // nejdřív starší zprávy z cache (i z indexace na pozadí), pak ze serveru
                let oldest = self
                    .messages
                    .get(&id)
                    .and_then(|v| v.iter().map(id_ms).filter(|n| *n > 0).min());
                if let (Some(oldest), Some((store, account))) = (oldest, self.store()) {
                    match store.load_messages_before(&account, &id, oldest, STORE_PAGE) {
                        Ok(page) if !page.is_empty() => {
                            if page.len() < STORE_PAGE {
                                self.older_in_store.remove(&id);
                            }
                            let media = self.fetch_media_for(&page);
                            merge_messages(self.messages.entry(id).or_default(), page);
                            return media;
                        }
                        Ok(_) => {
                            self.older_in_store.remove(&id);
                            if let Ok(Some(cursor)) = store.older_cursor(&account, &id) {
                                self.older.insert(id.clone(), cursor);
                            }
                        }
                        Err(e) => tracing::warn!(error = %e, "starší zprávy z cache"),
                    }
                }
                match self.older.get(&id).cloned().flatten() {
                    Some(cursor) if !self.loading_older => {
                        self.loading_older = true;
                        self.fetch_messages(id, Some(cursor))
                    }
                    _ => Task::none(),
                }
            }

            Msg::Search(s) if self.tab == Tab::Calendar => {
                self.search = s;
                Task::none()
            }
            Msg::Search(s) => {
                self.search = s;
                self.search_seq += 1;
                self.people_note = None;
                // hledat se má ve všech chatech, ne jen v načtených
                let load = if self.search.trim().is_empty() {
                    Task::none()
                } else {
                    self.want_all_chats()
                };
                if self.tab != Tab::Chats {
                    self.people.clear();
                    self.searching_people = false;
                }
                if self.search.trim().chars().count() < MIN_PEOPLE_QUERY {
                    self.people.clear();
                    self.searching_people = false;
                    self.message_hits.clear();
                    return load;
                }
                let seq = self.search_seq;
                Task::batch([
                    load,
                    Task::perform(tokio::time::sleep(SEARCH_DEBOUNCE), move |_| {
                        Msg::SearchFire(seq)
                    }),
                ])
            }
            Msg::SearchFire(seq) => {
                let Some(backend) = self.session().map(|s| s.backend.clone()) else {
                    return Task::none();
                };
                if seq != self.search_seq {
                    return Task::none();
                }
                let query = self.search.trim().to_owned();
                // zprávy: fulltext v cache (rychlý, přímo zde)
                self.message_hits = match self.store() {
                    Some((store, account)) => store
                        .search(&account, &query, None, MESSAGE_HITS)
                        .unwrap_or_else(|e| {
                            tracing::warn!(error = %e, "hledání ve zprávách");
                            vec![]
                        }),
                    None => vec![],
                };
                if self.tab != Tab::Chats {
                    return Task::none();
                }
                self.searching_people = true;
                self.people.clear();
                let mut tasks = vec![Task::perform(
                    {
                        let (backend, query) = (backend.clone(), query.clone());
                        async move { backend.search_people(&query).await.map_err(Failure::from) }
                    },
                    move |r| Msg::People(seq, r),
                )];
                if looks_like_email(&query) {
                    tasks.push(Task::perform(
                        async move { backend.find_external(&query).await.map_err(Failure::from) },
                        move |r| Msg::External(seq, r),
                    ));
                }
                Task::batch(tasks)
            }
            Msg::People(seq, r) | Msg::External(seq, r) if seq != self.search_seq => {
                let _ = r;
                Task::none()
            }
            Msg::People(_, Ok(found)) => {
                self.searching_people = false;
                self.add_people(found);
                Task::none()
            }
            Msg::External(_, Ok(found)) => {
                self.add_people(found);
                Task::none()
            }
            Msg::People(_, Err(e)) => {
                self.searching_people = false;
                self.fail(e)
            }
            Msg::External(_, Err(e)) => {
                tracing::warn!(error = %e.text, "hledání mimo organizaci");
                self.people_note =
                    Some("Hledání lidí mimo organizaci zatím není k dispozici.".into());
                Task::none()
            }
            Msg::OpenPerson(person) => self.open_person(person),
            Msg::NewGroup => {
                self.group_builder = Some(GroupBuilder::default());
                operation::focus(GROUP_QUERY)
            }
            Msg::GroupTopic(t) => {
                if let Some(g) = &mut self.group_builder {
                    g.topic = t;
                }
                Task::none()
            }
            Msg::GroupQuery(q) => {
                let Some(g) = &mut self.group_builder else {
                    return Task::none();
                };
                g.query = q;
                g.seq += 1;
                if g.query.trim().chars().count() < MIN_PEOPLE_QUERY {
                    g.results.clear();
                    g.searching = false;
                    return Task::none();
                }
                let seq = g.seq;
                Task::perform(tokio::time::sleep(SEARCH_DEBOUNCE), move |_| {
                    Msg::GroupSearchFire(seq)
                })
            }
            Msg::GroupSearchFire(seq) => {
                let backend = self.session().map(|s| s.backend.clone());
                let (Some(g), Some(backend)) = (&mut self.group_builder, backend) else {
                    return Task::none();
                };
                if seq != g.seq {
                    return Task::none();
                }
                g.searching = true;
                let query = g.query.trim().to_owned();
                Task::perform(
                    async move { backend.search_people(&query).await.map_err(Failure::from) },
                    move |r| Msg::GroupPeople(seq, r),
                )
            }
            Msg::GroupPeople(seq, r) => {
                let me = self.session().map(|s| s.profile.account_id.clone());
                let Some(g) = &mut self.group_builder else {
                    return Task::none();
                };
                if seq != g.seq {
                    return Task::none();
                }
                g.searching = false;
                match r {
                    Ok(found) => {
                        g.results = found
                            .into_iter()
                            .filter(|p| {
                                !me.as_ref()
                                    .is_some_and(|m| p.mri.as_str().ends_with(m.as_str()))
                            })
                            .collect();
                        Task::none()
                    }
                    Err(e) => self.fail(e),
                }
            }
            Msg::GroupAdd(person) => {
                if person.object_id().is_none() {
                    return self.show_toast(
                        "Tuto osobu zatím do chatu přidat neumím (účet mimo Microsoft 365)",
                    );
                }
                if let Some(g) = &mut self.group_builder {
                    if !g.members.iter().any(|m| m.mri == person.mri) {
                        g.members.push(person);
                    }
                    g.query.clear();
                    g.results.clear();
                    g.seq += 1;
                }
                operation::focus(GROUP_QUERY)
            }
            Msg::GroupRemove(mri) => {
                if let Some(g) = &mut self.group_builder {
                    g.members.retain(|m| m.mri != mri);
                }
                Task::none()
            }
            Msg::GroupCancel => {
                self.group_builder = None;
                Task::none()
            }
            Msg::GroupCreate => self.create_group_draft(),
            Msg::WindowResized(w) => {
                self.window_width = w;
                Task::none()
            }
            Msg::WindowFocus(f) => {
                self.focused = f;
                // návrat do okna: otevřený chat je teď opravdu přečtený
                if let Some(sel) = self.selected.clone() {
                    self.mark_read(&sel);
                }
                // návrat z prohlížeče: adresa stránky může být ve schránce
                if f && self.browser_login.is_some() && !self.redeeming {
                    return iced::clipboard::read().map(Msg::LoginClipboard);
                }
                Task::none()
            }
            Msg::NotificationClicked(id) => {
                self.tab = Tab::Chats;
                let select = self.update(Msg::Select(id));
                Task::batch([select, self.show_window()])
            }
            Msg::Noop => Task::none(),
            Msg::TrayReady(handle) => {
                self.tray = Some(handle);
                self.tray_unread = usize::MAX; // vynutí první aktualizaci
                Task::none()
            }
            Msg::TrayUnavailable => {
                self.tray = None;
                // bez ikony v liště by skryté okno nešlo otevřít
                if self.hidden {
                    return self.show_window();
                }
                Task::none()
            }
            Msg::Tray(TrayEvent::Show) => self.show_window(),
            Msg::Tray(TrayEvent::Quit) => iced::exit(),
            Msg::Tray(TrayEvent::OpenMeeting(i)) => match self.calendar.alerts.get(i).cloned() {
                Some((account, event)) => self.open_event(account, event.id),
                None => self.show_window(),
            },
            Msg::CloseRequested(id) => {
                if self.tray.is_some() {
                    // zavření okna = skrytí do lišty; ukončení je v menu ikony
                    self.hidden = true;
                    iced::window::set_mode(id, iced::window::Mode::Hidden)
                } else {
                    iced::exit()
                }
            }
            Msg::Paste => Task::perform(
                async {
                    tokio::task::spawn_blocking(clipboard::read)
                        .await
                        .unwrap_or(Clip::Empty)
                },
                Msg::Pasted,
            ),
            Msg::Pasted(Clip::Image { png, width, height }) => {
                let preview = image::Handle::from_bytes(png.as_ref().clone());
                self.pending_images.push(PendingImage {
                    png,
                    width,
                    height,
                    preview,
                });
                operation::focus(COMPOSER)
            }
            Msg::Pasted(Clip::Text(t)) => {
                self.composer
                    .perform(text_editor::Action::Edit(text_editor::Edit::Paste(
                        Arc::new(t),
                    )));
                Task::none()
            }
            Msg::Pasted(Clip::Empty) => Task::none(),
            Msg::RemovePending(i) => {
                if i < self.pending_images.len() {
                    self.pending_images.remove(i);
                }
                Task::none()
            }
            Msg::RemoveLastPending => {
                self.pending_images.pop();
                Task::none()
            }
            Msg::HoverMessage(id) => {
                self.hovered_message = Some(id);
                Task::none()
            }
            Msg::UnhoverMessage(id) => {
                if self.hovered_message.as_ref() == Some(&id) {
                    self.hovered_message = None;
                }
                Task::none()
            }
            Msg::CopyMessage(id) => match self.find_message(&id).map(message_plain) {
                Some(text) if !text.is_empty() => self.copy(text, "Zpráva zkopírována"),
                _ => Task::none(),
            },
            Msg::StartSelect(id) => {
                let Some(text) = self.find_message(&id).map(message_plain) else {
                    return Task::none();
                };
                if text.is_empty() {
                    return Task::none();
                }
                let width = estimate_text_width(&text, self.bubble_max() - BUBBLE_PADDING);
                self.selecting = Some(Selecting {
                    id,
                    content: text_editor::Content::with_text(&text),
                    width,
                });
                Task::none()
            }
            Msg::StopSelect => {
                self.selecting = None;
                Task::none()
            }
            Msg::Reply(id) => {
                let Some(m) = self.find_message(&id) else {
                    return Task::none();
                };
                let mut preview = message_plain(m);
                if preview.is_empty()
                    && message_parts(&m.content)
                        .iter()
                        .any(|p| matches!(p, Part::Image(_)))
                {
                    preview = "🖼 Obrázek".into();
                }
                self.replying = Some(ReplyRef {
                    message_id: m.id.clone(),
                    author: m.sender.clone(),
                    author_name: m.sender_name.clone().unwrap_or_else(|| "?".into()),
                    preview: truncate_chars(&preview.replace('\n', " "), REPLY_PREVIEW_CHARS),
                });
                self.selecting = None;
                operation::focus(COMPOSER)
            }
            Msg::CancelReply => {
                self.replying = None;
                Task::none()
            }
            Msg::ToggleReactions(id) => {
                self.reacting = if self.reacting.as_ref() == Some(&id) {
                    None
                } else {
                    Some(id)
                };
                Task::none()
            }
            Msg::React(id, key) => {
                self.reacting = None;
                let (Some(conv), Some(session)) = (self.selected.clone(), self.session().cloned())
                else {
                    return Task::none();
                };
                let me = my_mri(&session.profile);
                let account = session.profile.account_id.as_str().to_owned();
                let mut on = true;
                let changed = self.change_message(&conv, &id, |m| {
                    on = toggle_reaction(m, &key, &me, &account);
                });
                if changed.is_none() {
                    return Task::none();
                }
                let backend = session.backend.clone();
                Task::perform(
                    {
                        let conv = conv.clone();
                        async move {
                            backend
                                .react(&conv, &id, &key, on)
                                .await
                                .map_err(Failure::from)
                        }
                    },
                    move |r| Msg::Reacted(conv.clone(), r),
                )
            }
            Msg::Reacted(_, Ok(())) | Msg::Edited(_, Ok(())) | Msg::Deleted(_, Ok(())) => {
                Task::none()
            }
            Msg::Reacted(conv, Err(e)) | Msg::Edited(conv, Err(e)) | Msg::Deleted(conv, Err(e)) => {
                // místní změna neprošla: vrátit stav ze serveru
                let refresh = self.fetch_messages(conv, None);
                Task::batch([refresh, self.fail(e)])
            }
            Msg::StartEdit(id) => {
                let Some(conv) = self.selected.clone() else {
                    return Task::none();
                };
                let Some(m) = self.find_message(&id) else {
                    return Task::none();
                };
                let text = tymy_core::text::markdown::from_blocks(&rich::parse(&m.content));
                let original = m.content.clone();
                let draft = match &self.editing {
                    Some(ed) => ed.draft.clone(),
                    None => self.composer.text().trim_end_matches('\n').to_owned(),
                };
                self.editing = Some(Editing {
                    conversation: conv,
                    id,
                    original,
                    draft,
                });
                self.replying = None;
                self.selecting = None;
                self.confirm_delete = None;
                self.composer = text_editor::Content::with_text(&text);
                self.composer
                    .perform(text_editor::Action::Move(text_editor::Motion::DocumentEnd));
                operation::focus(COMPOSER)
            }
            Msg::EditLast => {
                let Some(profile) = self.session().map(|s| s.profile.clone()) else {
                    return Task::none();
                };
                let last = self
                    .selected
                    .as_ref()
                    .and_then(|c| self.messages.get(c))
                    .and_then(|v| {
                        v.iter()
                            .rev()
                            .find(|m| !m.deleted && m.is_user_content() && is_me(&profile, m))
                    })
                    .map(|m| m.id.clone());
                match last {
                    Some(id) => self.update(Msg::StartEdit(id)),
                    None => Task::none(),
                }
            }
            Msg::CancelEdit => {
                self.cancel_edit();
                Task::none()
            }
            Msg::AskDelete(id) => {
                self.confirm_delete = Some(id);
                self.reacting = None;
                Task::none()
            }
            Msg::CancelDelete => {
                self.confirm_delete = None;
                Task::none()
            }
            Msg::Delete(id) => {
                self.confirm_delete = None;
                let (Some(conv), Some(backend)) = (
                    self.selected.clone(),
                    self.session().map(|s| s.backend.clone()),
                ) else {
                    return Task::none();
                };
                if self.editing.as_ref().is_some_and(|e| e.id == id) {
                    self.cancel_edit();
                }
                if self
                    .change_message(&conv, &id, |m| m.deleted = true)
                    .is_none()
                {
                    return Task::none();
                }
                Task::perform(
                    {
                        let conv = conv.clone();
                        async move { backend.delete(&conv, &id).await.map_err(Failure::from) }
                    },
                    move |r| Msg::Deleted(conv.clone(), r),
                )
            }
            Msg::CopyText(value, toast) => self.copy(value, toast),
            Msg::OpenHit(conv, id) => self.open_hit(conv, id),
            Msg::JumpToQuoted(id) => self.jump_to_quoted(id),
            Msg::IndexNext => self.index_next(),
            Msg::Indexed(conv, Ok(page)) => {
                if let Some((store, account)) = self.store() {
                    if let Err(e) = store.save_messages(&account, &page.messages) {
                        tracing::warn!(error = %e, "indexace: uložení zpráv");
                    }
                    if let Err(e) = store.set_older_cursor(&account, &conv, page.older.as_deref()) {
                        tracing::warn!(error = %e, "indexace: uložení kurzoru");
                    }
                }
                if let Some(s) = self.session().cloned() {
                    self.peers.learn(&s.profile, page.messages.iter().rev());
                }
                if self.messages.contains_key(&conv) {
                    self.older_in_store.insert(conv);
                }
                Task::perform(tokio::time::sleep(INDEX_DELAY), |_| Msg::IndexNext)
            }
            Msg::Indexed(conv, Err(e)) => {
                tracing::warn!(%conv, error = %e.text, "indexace konverzace selhala, přeskakuji");
                self.index_queue.retain(|c| c != &conv);
                if e.forbidden {
                    self.mark_forbidden(conv.clone(), &e);
                }
                if e.reauth {
                    self.index_running = false;
                    return self.fail(e);
                }
                Task::perform(tokio::time::sleep(INDEX_DELAY), |_| Msg::IndexNext)
            }
            Msg::SelectAction(action) => {
                let Some(sel) = &mut self.selecting else {
                    return Task::none();
                };
                // jen pro čtení: výběr a pohyb ano, úpravy ne
                if action.is_edit() {
                    return Task::none();
                }
                sel.content.perform(action);
                self.selection_seq += 1;
                if sel.content.selection().is_some_and(|t| !t.is_empty()) {
                    let seq = self.selection_seq;
                    return Task::perform(tokio::time::sleep(COPY_DELAY), move |_| {
                        Msg::CopySelection(seq)
                    });
                }
                Task::none()
            }
            Msg::CopySelection(seq) => {
                if seq != self.selection_seq {
                    return Task::none();
                }
                match self.selecting.as_ref().and_then(|s| s.content.selection()) {
                    Some(text) if !text.is_empty() => self.copy(text, "Výběr zkopírován"),
                    _ => Task::none(),
                }
            }
            Msg::HideToast(seq) => {
                if seq == self.toast_seq {
                    self.toast = None;
                }
                Task::none()
            }
            Msg::ToggleTheme => {
                self.theme_mode = self.theme_mode.next();
                match Config::load() {
                    Ok(mut c) => {
                        c.settings.theme = self.theme_mode;
                        if let Err(e) = c.save_settings() {
                            tracing::warn!(error = %e, "uložení vzhledu");
                        }
                    }
                    Err(e) => tracing::warn!(error = %e, "načtení nastavení"),
                }
                Task::none()
            }
            Msg::SystemTheme(mode) => {
                self.system_dark = mode == iced::theme::Mode::Dark;
                Task::none()
            }
            Msg::SetTab(tab) => {
                self.tab = tab;
                self.search.clear();
                self.people.clear();
                self.people_note = None;
                self.search_seq += 1;
                // kanály s dávnou aktivitou jsou až na dalších stránkách seznamu
                match tab {
                    Tab::Teams => self.want_all_chats(),
                    Tab::Calendar => {
                        // otevřený kalendář = připomenuté schůzky účtu viděné
                        if let Some(account) = self.active_account().cloned() {
                            let ids: Vec<String> = self
                                .calendar
                                .alerts
                                .iter()
                                .filter(|(a, _)| *a == account)
                                .map(|(_, e)| e.id.clone())
                                .collect();
                            for id in ids {
                                self.ack_meeting(&account, &id);
                            }
                        }
                        Task::batch([
                            self.refresh_calendar(chrono::Duration::minutes(1)),
                            self.fetch_week(chrono::Duration::minutes(1)),
                            self.scroll_week(None),
                        ])
                    }
                    Tab::Chats => Task::none(),
                }
            }
            Msg::Hover(id) => {
                self.hovered = Some(id);
                Task::none()
            }
            Msg::Unhover(id) => {
                if self.hovered.as_ref() == Some(&id) {
                    self.hovered = None;
                }
                Task::none()
            }
            Msg::Pin(id) => {
                if !self.pins.contains(&id) {
                    self.pins.push(id);
                    self.save_pins();
                }
                Task::none()
            }
            Msg::Unpin(id) => {
                self.pins.retain(|p| p != &id);
                self.save_pins();
                Task::none()
            }
            Msg::MovePin(id, delta) => {
                move_pin(&mut self.pins, &id, delta);
                self.save_pins();
                Task::none()
            }
            Msg::Compose(action) => {
                self.composer.perform(action);
                self.refresh_mention()
            }
            Msg::WeekShift(delta) => {
                self.week = if delta == 0 {
                    cal::week_start(Local::now().date_naive())
                } else {
                    self.week + chrono::Duration::weeks(delta)
                };
                let fetch = self.fetch_week(chrono::Duration::minutes(1));
                Task::batch([fetch, self.scroll_week(None)])
            }
            Msg::GridMove(day, y) => {
                let minutes = ((y / WEEK_HOUR * 60.0) as i64).clamp(0, 24 * 60);
                self.grid_hover = Some((day, minutes));
                // táhne se jen v rámci dne, kde začal výběr (i když myš přejde jinam)
                if let Some(d) = &mut self.grid_drag {
                    d.current = minutes;
                }
                Task::none()
            }
            Msg::GridPress(day) => {
                if let Some((d, minutes)) = self.grid_hover.filter(|(d, _)| *d == day) {
                    self.grid_drag = Some(GridDrag {
                        day: d,
                        anchor: minutes,
                        current: minutes,
                    });
                }
                Task::none()
            }
            Msg::MouseUp => {
                let Some(drag) = self.grid_drag.take() else {
                    return Task::none();
                };
                let (from, to) = cal::drag_range(drag.anchor, drag.current);
                let day = self.week + chrono::Duration::days(drag.day as i64);
                let open = self.update(Msg::NewMeeting);
                if let Some(f) = &mut self.meeting_form {
                    f.date = day.format("%-d. %-m. %Y").to_string();
                    f.start = cal::minutes_time(from).format("%H:%M").to_string();
                    f.end = cal::minutes_time(to).format("%H:%M").to_string();
                }
                open
            }
            Msg::WeekLoaded(account, week, r) => {
                let key = (account, week);
                self.week_loading.remove(&key);
                self.week_fetched.insert(key.clone(), Utc::now());
                match r {
                    Ok(events) => {
                        self.week_cache.insert(key, events);
                    }
                    Err(e) => tracing::warn!(error = %e.text, "týden kalendáře nelze načíst"),
                }
                Task::none()
            }
            Msg::CloseEvent => {
                self.calendar.selected = None;
                self.confirm_cancel_event = None;
                Task::none()
            }
            Msg::Escape => {
                if self.tab == Tab::Calendar
                    && self.meeting_form.is_none()
                    && self.calendar.selected.is_some()
                {
                    self.calendar.selected = None;
                    self.confirm_cancel_event = None;
                }
                Task::none()
            }
            Msg::NewMeeting => {
                let (day, start, end) = cal::default_slot(Utc::now());
                self.meeting_form = Some(MeetingForm {
                    subject: String::new(),
                    date: day.format("%-d. %-m. %Y").to_string(),
                    start: start.format("%H:%M").to_string(),
                    end: end.format("%H:%M").to_string(),
                    location: String::new(),
                    body: String::new(),
                    online: true,
                    attendees: vec![],
                    query: String::new(),
                    seq: 0,
                    searching: false,
                    results: vec![],
                    busy: false,
                    error: None,
                });
                Task::none()
            }
            Msg::MeetingInput(field, value) => {
                let Some(f) = &mut self.meeting_form else {
                    return Task::none();
                };
                f.error = None;
                match field {
                    MeetingField::Subject => f.subject = value,
                    MeetingField::Date => f.date = value,
                    MeetingField::Start => f.start = value,
                    MeetingField::End => f.end = value,
                    MeetingField::Location => f.location = value,
                    MeetingField::Body => f.body = value,
                    MeetingField::Query => {
                        f.query = value;
                        f.seq += 1;
                        if f.query.trim().chars().count() < MIN_PEOPLE_QUERY {
                            f.results.clear();
                            f.searching = false;
                            return Task::none();
                        }
                        let seq = f.seq;
                        return Task::perform(tokio::time::sleep(SEARCH_DEBOUNCE), move |_| {
                            Msg::MeetingSearchFire(seq)
                        });
                    }
                }
                Task::none()
            }
            Msg::MeetingOnline(on) => {
                if let Some(f) = &mut self.meeting_form {
                    f.online = on;
                }
                Task::none()
            }
            Msg::MeetingSearchFire(seq) => {
                let backend = self.session().map(|s| s.backend.clone());
                let (Some(f), Some(backend)) = (&mut self.meeting_form, backend) else {
                    return Task::none();
                };
                if seq != f.seq {
                    return Task::none();
                }
                f.searching = true;
                let query = f.query.trim().to_owned();
                Task::perform(
                    async move { backend.search_people(&query).await.map_err(Failure::from) },
                    move |r| Msg::MeetingPeople(seq, r),
                )
            }
            Msg::MeetingPeople(seq, r) => {
                let Some(f) = &mut self.meeting_form else {
                    return Task::none();
                };
                if seq != f.seq {
                    return Task::none();
                }
                f.searching = false;
                match r {
                    Ok(found) => {
                        f.results = found;
                        Task::none()
                    }
                    Err(e) => self.fail(e),
                }
            }
            Msg::MeetingAdd(person) => {
                if let Some(f) = &mut self.meeting_form
                    && !f.attendees.iter().any(|a| a.mri == person.mri)
                {
                    f.attendees.push(person);
                    f.query.clear();
                    f.results.clear();
                }
                Task::none()
            }
            Msg::MeetingRemove(mri) => {
                if let Some(f) = &mut self.meeting_form {
                    f.attendees.retain(|a| a.mri != mri);
                }
                Task::none()
            }
            Msg::MeetingClose => {
                self.meeting_form = None;
                Task::none()
            }
            Msg::MeetingCreate => {
                let (Some(s), Some(f)) = (self.session().cloned(), &mut self.meeting_form) else {
                    return Task::none();
                };
                if f.busy {
                    return Task::none();
                }
                let meeting = match meeting_from_form(f) {
                    Ok(m) => m,
                    Err(e) => {
                        f.error = Some(e);
                        return Task::none();
                    }
                };
                f.busy = true;
                let account = s.profile.account_id.clone();
                tracing::info!(
                    attendees = meeting.attendees.len(),
                    online = meeting.online,
                    "plánuji schůzku"
                );
                Task::perform(
                    async move {
                        s.backend
                            .create_meeting(&meeting)
                            .await
                            .map_err(Failure::from)
                    },
                    move |r| Msg::MeetingCreated(account.clone(), r),
                )
            }
            Msg::MeetingCreated(account, r) => {
                if let Some(f) = &mut self.meeting_form {
                    f.busy = false;
                }
                match r {
                    Ok(id) => {
                        self.meeting_form = None;
                        self.calendar.selected = Some(id);
                        self.calendar.fetched.remove(&account);
                        let refresh = Task::batch([
                            self.refresh_calendar(chrono::Duration::zero()),
                            self.fetch_week(chrono::Duration::zero()),
                        ]);
                        Task::batch([refresh, self.show_toast("Schůzka naplánována")])
                    }
                    Err(e) => {
                        if let Some(f) = &mut self.meeting_form {
                            f.error = Some(e.text.clone());
                        }
                        tracing::warn!(error = %e.text, "plánování schůzky");
                        Task::none()
                    }
                }
            }
            Msg::AskCancelEvent(id) => {
                self.confirm_cancel_event = Some(id);
                Task::none()
            }
            Msg::KeepEvent => {
                self.confirm_cancel_event = None;
                Task::none()
            }
            Msg::CancelEvent(id) => {
                let Some(backend) = self.session().map(|s| s.backend.clone()) else {
                    return Task::none();
                };
                if self.event_busy {
                    return Task::none();
                }
                self.event_busy = true;
                self.confirm_cancel_event = None;
                Task::perform(
                    {
                        let id = id.clone();
                        async move {
                            backend
                                .cancel_meeting(&id, "Schůzka je zrušena.")
                                .await
                                .map_err(Failure::from)
                        }
                    },
                    move |r| Msg::EventCancelled(id.clone(), r),
                )
            }
            Msg::EventCancelled(id, r) => {
                self.event_busy = false;
                match r {
                    Ok(()) => {
                        self.update_active_event(&id, |e| e.cancelled = true);
                        if let Some(account) = self.active_account().cloned() {
                            self.calendar.fetched.remove(&account);
                        }
                        let refresh = Task::batch([
                            self.refresh_calendar(chrono::Duration::zero()),
                            self.fetch_week(chrono::Duration::zero()),
                        ]);
                        Task::batch([refresh, self.show_toast("Schůzka zrušena")])
                    }
                    Err(e) => self.fail(e),
                }
            }
            Msg::ReplyEvent(id, reply) => {
                let Some(backend) = self.session().map(|s| s.backend.clone()) else {
                    return Task::none();
                };
                if self.event_busy {
                    return Task::none();
                }
                self.event_busy = true;
                Task::perform(
                    {
                        let id = id.clone();
                        async move {
                            backend
                                .reply_to_event(&id, reply)
                                .await
                                .map_err(Failure::from)
                        }
                    },
                    move |r| Msg::EventReplied(id.clone(), reply, r),
                )
            }
            Msg::EventReplied(id, reply, r) => {
                self.event_busy = false;
                match r {
                    Ok(()) => {
                        self.update_active_event(&id, |e| {
                            e.response = match reply {
                                EventReply::Accept => EventResponse::Accepted,
                                EventReply::Tentative => EventResponse::Tentative,
                                EventReply::Decline => EventResponse::Declined,
                            }
                        });
                        self.show_toast(match reply {
                            EventReply::Accept => "Pozvánka přijata",
                            EventReply::Tentative => "Předběžně přijato",
                            EventReply::Decline => "Pozvánka odmítnuta",
                        })
                    }
                    Err(e) => self.fail(e),
                }
            }
            Msg::ToggleChatPanel => {
                if self.chat_panel.take().is_some() {
                    return Task::none();
                }
                let Some(id) = self.selected.clone() else {
                    return Task::none();
                };
                let topic = self
                    .conversations
                    .iter()
                    .find(|c| c.id == id)
                    .and_then(|c| c.topic.clone())
                    .unwrap_or_default();
                self.chat_panel = Some(ChatPanel {
                    conv: id.clone(),
                    topic,
                    query: String::new(),
                    seq: 0,
                    searching: false,
                    results: vec![],
                    busy: false,
                    confirm_leave: false,
                    confirm_remove: None,
                });
                // členové vždy čerstvě
                self.rosters.remove(&id);
                self.fetch_roster()
            }
            Msg::PanelTopic(t) => {
                if let Some(p) = &mut self.chat_panel {
                    p.topic = t;
                }
                Task::none()
            }
            Msg::SaveTopic => {
                let backend = self.session().map(|s| s.backend.clone());
                let (Some(p), Some(backend)) = (&mut self.chat_panel, backend) else {
                    return Task::none();
                };
                if p.busy {
                    return Task::none();
                }
                p.busy = true;
                let (conv, topic) = (p.conv.clone(), p.topic.trim().to_owned());
                Task::perform(
                    {
                        let (conv, topic) = (conv.clone(), topic.clone());
                        async move {
                            backend
                                .set_topic(&conv, &topic)
                                .await
                                .map_err(Failure::from)
                        }
                    },
                    move |r| Msg::TopicSaved(conv.clone(), topic.clone(), r),
                )
            }
            Msg::TopicSaved(conv, topic, r) => {
                if let Some(p) = &mut self.chat_panel {
                    p.busy = false;
                }
                match r {
                    Ok(()) => {
                        if let Some(c) = self.conversations.iter_mut().find(|c| c.id == conv) {
                            c.topic = (!topic.is_empty()).then_some(topic);
                        }
                        self.show_toast("Chat přejmenován")
                    }
                    Err(e) => self.fail(e),
                }
            }
            Msg::PanelQuery(q) => {
                let Some(p) = &mut self.chat_panel else {
                    return Task::none();
                };
                p.query = q;
                p.seq += 1;
                if p.query.trim().chars().count() < MIN_PEOPLE_QUERY {
                    p.results.clear();
                    p.searching = false;
                    return Task::none();
                }
                let seq = p.seq;
                Task::perform(tokio::time::sleep(SEARCH_DEBOUNCE), move |_| {
                    Msg::PanelSearchFire(seq)
                })
            }
            Msg::PanelSearchFire(seq) => {
                let backend = self.session().map(|s| s.backend.clone());
                let (Some(p), Some(backend)) = (&mut self.chat_panel, backend) else {
                    return Task::none();
                };
                if seq != p.seq {
                    return Task::none();
                }
                p.searching = true;
                let query = p.query.trim().to_owned();
                Task::perform(
                    async move { backend.search_people(&query).await.map_err(Failure::from) },
                    move |r| Msg::PanelPeople(seq, r),
                )
            }
            Msg::PanelPeople(seq, r) => {
                let Some(p) = &mut self.chat_panel else {
                    return Task::none();
                };
                if seq != p.seq {
                    return Task::none();
                }
                p.searching = false;
                match r {
                    Ok(found) => {
                        p.results = found;
                        Task::none()
                    }
                    Err(e) => self.fail(e),
                }
            }
            Msg::AddMember(person) => {
                let backend = self.session().map(|s| s.backend.clone());
                let (Some(p), Some(backend)) = (&mut self.chat_panel, backend) else {
                    return Task::none();
                };
                if p.busy {
                    return Task::none();
                }
                p.busy = true;
                let conv = p.conv.clone();
                Task::perform(
                    {
                        let (conv, mri) = (conv.clone(), person.mri.clone());
                        async move {
                            backend
                                .add_members(&conv, &[mri])
                                .await
                                .map_err(Failure::from)
                        }
                    },
                    move |r| Msg::MemberAdded(conv.clone(), person.clone(), r),
                )
            }
            Msg::MemberAdded(conv, person, r) => {
                if let Some(p) = &mut self.chat_panel {
                    p.busy = false;
                    if r.is_ok() {
                        p.query.clear();
                        p.results.clear();
                    }
                }
                match r {
                    Ok(()) => {
                        let toast = format!("{} přidán(a) do chatu", person.display_name);
                        let roster = self.rosters.entry(conv).or_default();
                        if !roster.iter().any(|x| x.mri == person.mri) {
                            roster.push(person);
                        }
                        self.show_toast(&toast)
                    }
                    Err(e) => self.fail(e),
                }
            }
            Msg::RemoveMember(mri) => {
                let backend = self.session().map(|s| s.backend.clone());
                let (Some(p), Some(backend)) = (&mut self.chat_panel, backend) else {
                    return Task::none();
                };
                if p.busy {
                    return Task::none();
                }
                // první klik jen požádá o potvrzení
                if p.confirm_remove.as_ref() != Some(&mri) {
                    p.confirm_remove = Some(mri);
                    return Task::none();
                }
                p.confirm_remove = None;
                p.busy = true;
                let conv = p.conv.clone();
                Task::perform(
                    {
                        let (conv, mri) = (conv.clone(), mri.clone());
                        async move {
                            backend
                                .remove_member(&conv, &mri)
                                .await
                                .map_err(Failure::from)
                        }
                    },
                    move |r| Msg::MemberRemoved(conv.clone(), mri.clone(), r),
                )
            }
            Msg::MemberRemoved(conv, mri, r) => {
                if let Some(p) = &mut self.chat_panel {
                    p.busy = false;
                }
                match r {
                    Ok(()) => {
                        if let Some(roster) = self.rosters.get_mut(&conv) {
                            roster.retain(|x| x.mri != mri);
                        }
                        self.show_toast("Odebráno z chatu")
                    }
                    Err(e) => self.fail(e),
                }
            }
            Msg::AskLeave => {
                if let Some(p) = &mut self.chat_panel {
                    p.confirm_leave = true;
                }
                Task::none()
            }
            Msg::CancelLeave => {
                if let Some(p) = &mut self.chat_panel {
                    p.confirm_leave = false;
                }
                Task::none()
            }
            Msg::Leave => {
                let backend = self.session().map(|s| s.backend.clone());
                let (Some(p), Some(backend)) = (&mut self.chat_panel, backend) else {
                    return Task::none();
                };
                if p.busy {
                    return Task::none();
                }
                p.busy = true;
                let conv = p.conv.clone();
                Task::perform(
                    {
                        let conv = conv.clone();
                        async move { backend.leave(&conv).await.map_err(Failure::from) }
                    },
                    move |r| Msg::Left(conv.clone(), r),
                )
            }
            Msg::Left(conv, r) => {
                if let Some(p) = &mut self.chat_panel {
                    p.busy = false;
                    p.confirm_leave = false;
                }
                match r {
                    Ok(()) => {
                        tracing::info!(%conv, "chat opuštěn");
                        self.chat_panel = None;
                        // v seznamu už neukazovat (server ho může ještě chvíli vracet)
                        self.forbidden.insert(conv.clone());
                        if self.selected.as_ref() == Some(&conv) {
                            self.selected = None;
                        }
                        self.show_toast("Opustili jste chat")
                    }
                    Err(e) => self.fail(e),
                }
            }
            Msg::ToggleEmoji => {
                self.emoji_open = !self.emoji_open;
                if self.emoji_open {
                    self.emoji_tab = if self.recent_emoji.is_empty() {
                        Some(0)
                    } else {
                        None
                    };
                }
                operation::focus(COMPOSER)
            }
            Msg::EmojiTab(tab) => {
                self.emoji_tab = tab;
                Task::none()
            }
            Msg::InsertEmoji(e) => {
                self.composer
                    .perform(text_editor::Action::Edit(text_editor::Edit::Paste(
                        Arc::new(e.to_owned()),
                    )));
                emoji::remember(&mut self.recent_emoji, e);
                operation::focus(COMPOSER)
            }
            Msg::MentionMove(delta) => {
                let n = self.mention_candidates().len();
                if n > 0 {
                    self.mention_index =
                        (self.mention_index as isize + delta).rem_euclid(n as isize) as usize;
                }
                Task::none()
            }
            Msg::MentionPick(index) => {
                let candidates = self.mention_candidates();
                let Some(q) = self.mention.take() else {
                    return Task::none();
                };
                let Some(person) = candidates.get(index.unwrap_or(self.mention_index)).cloned()
                else {
                    return Task::none();
                };
                // nahradit „@dotaz“ za „@Jméno “
                for _ in 0..=q.text.chars().count() {
                    self.composer
                        .perform(text_editor::Action::Select(text_editor::Motion::Left));
                }
                self.composer
                    .perform(text_editor::Action::Edit(text_editor::Edit::Paste(
                        Arc::new(format!("@{} ", person.name)),
                    )));
                if !self.composer_mentions.iter().any(|m| m.mri == person.mri) {
                    self.composer_mentions.push(person);
                }
                operation::focus(COMPOSER)
            }
            Msg::MentionClose => {
                if let Some(q) = self.mention.take() {
                    self.mention_dismissed = Some((q.line, q.at));
                }
                Task::none()
            }
            Msg::Roster(id, r) => {
                self.roster_loading.remove(&id);
                match r {
                    Ok(people) => {
                        self.rosters.insert(id, people);
                    }
                    Err(e) => {
                        tracing::warn!(%id, error = %e.text, "členové chatu pro zmínky");
                        // záloha: odesílatelé zpráv; znovu nezkoušet
                        self.rosters.insert(id, vec![]);
                    }
                }
                Task::none()
            }
            Msg::Send => {
                if let Some(ed) = self.editing.clone() {
                    return self.submit_edit(ed);
                }
                self.highlight = None;
                let text = self.composer.text().trim().to_owned();
                let (Some(id), Some(backend)) = (
                    self.selected.clone(),
                    self.session().map(|s| s.backend.clone()),
                ) else {
                    return Task::none();
                };
                if (text.is_empty() && self.pending_images.is_empty()) || self.sending {
                    return Task::none();
                }
                self.sending = true;
                self.composer = text_editor::Content::new();
                self.in_flight_images = std::mem::take(&mut self.pending_images);
                self.in_flight_reply = self.replying.take();
                let reply_to = self.in_flight_reply.clone();
                self.mention = None;
                self.emoji_open = false;
                self.in_flight_mentions = std::mem::take(&mut self.composer_mentions);
                let mentions: Vec<MentionRef> = self
                    .in_flight_mentions
                    .iter()
                    .filter(|m| text.contains(&format!("@{}", m.name)))
                    .cloned()
                    .collect();
                let images: Vec<OutgoingImage> = self
                    .in_flight_images
                    .iter()
                    .enumerate()
                    .map(|(i, p)| {
                        let (width, height) = clipboard::display_size(p.width, p.height);
                        OutgoingImage {
                            bytes: p.png.as_ref().clone(),
                            width,
                            height,
                            filename: format!("obrazek-{}.png", i + 1),
                        }
                    })
                    .collect();
                let outgoing = Arc::new(Outgoing {
                    text: text.clone(),
                    images,
                    reply_to,
                    mentions,
                });
                // U nového chatu: když vlákno neexistuje, založit ho a poslat znovu.
                let draft_peer = self.draft_people.get(&id).map(|p| p.mri.clone());
                let draft_group = self.draft_groups.get(&id).cloned();
                Task::perform(
                    {
                        let id = id.clone();
                        let deliver = async move {
                            let send = |to: ConversationId| {
                                let (backend, outgoing) = (backend.clone(), outgoing.clone());
                                async move { backend.send(&to, &outgoing).await }
                            };
                            if let Some(g) = draft_group {
                                tracing::info!(
                                    members = g.members.len(),
                                    "zakládám skupinový chat"
                                );
                                let created =
                                    backend.create_chat(&g.members, g.topic.as_deref()).await?;
                                send(created.clone()).await?;
                                return Ok(created);
                            }
                            match send(id.clone()).await {
                                Ok(_) => Ok(id),
                                Err(Error::Http { status: 404, .. }) if draft_peer.is_some() => {
                                    let peer = draft_peer.expect("ověřeno výše");
                                    tracing::info!(%id, "vlákno neexistuje, zakládám chat");
                                    let created = backend.create_chat(&[peer], None).await?;
                                    send(created.clone()).await?;
                                    Ok(created)
                                }
                                Err(e) => Err(e),
                            }
                        };
                        async move { deliver.await.map_err(Failure::from) }
                    },
                    move |r| Msg::Sent(id.clone(), text.clone(), r),
                )
            }
            Msg::Sent(requested, _, Ok(id)) => {
                self.sending = false;
                self.in_flight_mentions.clear();
                self.in_flight_images.clear();
                self.in_flight_reply = None;
                if id != requested {
                    // nový chat dostal od serveru jiné id než odhadnuté
                    if let Some(person) = self.draft_people.remove(&requested) {
                        self.peers.set(&id, &person.display_name);
                    }
                    if let Some(g) = self.draft_groups.remove(&requested) {
                        for n in g.names.iter().rev() {
                            self.peers.set(&id, n);
                        }
                    }
                    // chat zůstane vidět pod skutečným id, dokud ho nevrátí seznam ze serveru
                    if let Some(mut draft) =
                        self.draft_chats.iter().find(|d| d.id == requested).cloned()
                    {
                        draft.id = id.clone();
                        self.draft_chats.push(draft);
                    }
                    self.draft_chats.retain(|d| d.id != requested);
                    self.conversations.retain(|c| c.id != requested);
                    self.merge_draft_chats();
                    self.selected = Some(id.clone());
                }
                // vlastní zprávu vždy ukázat
                self.at_bottom = true;
                self.new_below = false;
                Task::batch([
                    self.fetch_messages(id, None),
                    operation::snap_to_end(MESSAGES),
                ])
            }
            Msg::Sent(requested, text, Err(e)) => {
                self.sending = false;
                let images = std::mem::take(&mut self.in_flight_images);
                let reply = self.in_flight_reply.take();
                let mentions = std::mem::take(&mut self.in_flight_mentions);
                if self.selected.as_ref() != Some(&requested) {
                    // uživatel mezitím přešel jinam: vrátit do konceptu toho chatu
                    let d = self.drafts.entry(requested).or_default();
                    if d.text.trim().is_empty() {
                        d.text = text;
                    }
                    let mut back = images;
                    back.append(&mut d.images);
                    d.images = back;
                    if d.reply.is_none() {
                        d.reply = reply;
                    }
                    d.mentions.extend(mentions);
                    return self.fail(e);
                }
                // vrátit i obrázky
                let mut back = images;
                back.append(&mut self.pending_images);
                self.pending_images = back;
                if self.replying.is_none() {
                    self.replying = reply;
                }
                self.composer_mentions.extend(mentions);
                // vrátit text, ať se neztratí
                if self.composer.text().trim().is_empty() {
                    self.composer = text_editor::Content::with_text(&text);
                }
                self.fail(e)
            }
            Msg::Scrolled(viewport) => {
                let offset = viewport.absolute_offset().y;
                let visible = viewport.bounds().height;
                let content = viewport.content_bounds().height;
                self.at_bottom = offset + visible >= content - BOTTOM_TOLERANCE;
                if self.at_bottom {
                    self.new_below = false;
                }
                Task::none()
            }
            Msg::JumpToNewest => {
                self.at_bottom = true;
                self.new_below = false;
                operation::snap_to_end(MESSAGES)
            }
            Msg::DismissError => {
                self.error = None;
                Task::none()
            }
            Msg::MediaLoaded(url, Ok(bytes)) => {
                if let Some((store, account)) = self.store()
                    && let Err(e) = store.save_media(&account, &url, &bytes)
                {
                    tracing::warn!(error = %e, "uložení obrázku do cache");
                }
                let handle = image::Handle::from_bytes(bytes.as_ref().clone());
                self.media.insert(url, Media::Ready(handle, bytes));
                // obrázek zvětšil obsah; kdo byl dole, zůstane dole
                if self.at_bottom {
                    operation::snap_to_end(MESSAGES)
                } else {
                    Task::none()
                }
            }
            Msg::MediaLoaded(url, Err(e)) => {
                tracing::warn!(%url, error = %e.text, "obrázek se nepodařilo stáhnout");
                self.media.insert(url, Media::Failed);
                Task::none()
            }
            Msg::OpenImage(img) => {
                let full = img.full_size_url();
                if let Some(path) = self.originals.get(&full).filter(|p| p.exists()) {
                    open_path(path);
                    return Task::none();
                }
                if let Some((store, account)) = self.store()
                    && let Ok(Some(bytes)) = store.load_media(&account, &full)
                {
                    return self.update(Msg::FullImage(full, img.url, Ok(Arc::new(bytes))));
                }
                let Some(backend) = self.session().map(|s| s.backend.clone()) else {
                    return Task::none();
                };
                let preview = img.url;
                Task::perform(
                    {
                        let full = full.clone();
                        async move {
                            backend
                                .fetch_media(&full)
                                .await
                                .map(Arc::new)
                                .map_err(Failure::from)
                        }
                    },
                    move |r| Msg::FullImage(full.clone(), preview.clone(), r),
                )
            }
            Msg::FullImage(full, _, Ok(bytes)) => {
                if let Some((store, account)) = self.store()
                    && let Err(e) = store.save_media(&account, &full, &bytes)
                {
                    tracing::warn!(error = %e, "uložení originálu do cache");
                }
                match write_temp_media(&full, &bytes) {
                    Ok(path) => {
                        open_path(&path);
                        self.originals.insert(full, path);
                    }
                    Err(e) => tracing::warn!(error = %e, "uložení obrázku selhalo"),
                }
                Task::none()
            }
            Msg::FullImage(_, preview, Err(e)) => {
                // Originál není k dispozici → otevřít aspoň náhled.
                tracing::warn!(error = %e.text, "originál obrázku nelze stáhnout, otevírám náhled");
                if let Some(Media::Ready(_, bytes)) = self.media.get(&preview) {
                    match write_temp_media(&preview, bytes) {
                        Ok(path) => open_path(&path),
                        Err(e) => tracing::warn!(error = %e, "uložení náhledu selhalo"),
                    }
                }
                Task::none()
            }
        }
    }

    pub fn view(&self) -> Element<'_, Msg> {
        let body: Element<'_, Msg> = match &self.phase {
            Phase::Starting => centered(text("Načítám…").size(15).style(muted)),
            Phase::Login { code, busy, back } => {
                self.login_view(code.as_ref(), *busy, back.as_ref())
            }
            Phase::Ready(session) => {
                let pane = if self.tab == Tab::Calendar {
                    self.calendar_pane()
                } else {
                    self.chat_pane(session)
                };
                row![self.sidebar(session), vline(), pane].into()
            }
        };
        let body = match &self.error {
            Some(err) => column![error_banner(err), body].into(),
            None => body,
        };
        container(body)
            .width(Length::Fill)
            .height(Length::Fill)
            .style(style::app_background)
            .into()
    }

    fn login_view<'a>(
        &'a self,
        code: Option<&DeviceCode>,
        busy: bool,
        back: Option<&'a Session>,
    ) -> Element<'a, Msg> {
        let content: Element<'_, Msg> = match code {
            _ if self.browser_login.is_some() => self.browser_login_view(),
            Some(code) => column![
                text("Přihlášení").size(22).font(BOLD),
                text("1. Otevřete přihlašovací stránku Microsoftu").style(muted),
                row![
                    button(text(code.verification_uri.clone()).size(14))
                        .padding(0)
                        .style(|t: &Theme, s| style::link(t, s, false))
                        .on_press(Msg::OpenUrl(code.verification_uri.clone())),
                    copy_button(&code.verification_uri, "Odkaz zkopírován"),
                ]
                .spacing(12)
                .align_y(iced::Alignment::Center),
                space().height(4),
                text("2. Zadejte tento kód").style(muted),
                row![
                    container(text(code.user_code.clone()).size(28).font(Font::MONOSPACE))
                        .padding([10, 18])
                        .style(style::day_pill),
                    copy_button(&code.user_code, "Kód zkopírován"),
                ]
                .spacing(12)
                .align_y(iced::Alignment::Center),
                text(self.toast.clone().unwrap_or_default())
                    .size(12)
                    .style(style::accent),
                text("3. Potvrďte přihlášení. Okno pak pokračuje samo.")
                    .size(13)
                    .style(muted),
                space().height(4),
                button(
                    text("Organizace přihlášení kódem blokuje? Přihlásit v prohlížeči").size(12)
                )
                .padding(0)
                .style(|t: &Theme, s| style::link(t, s, false))
                .on_press(Msg::StartBrowserLogin(self.login_kind)),
            ]
            .spacing(10)
            .into(),
            None => column![
                row![logo(40.0), text("Tymy").size(30).font(BOLD)]
                    .spacing(12)
                    .align_y(iced::Alignment::Center),
                text(if back.is_some() {
                    "Přihlášení dalšího účtu"
                } else {
                    "Lehký klient Teams pro Linux"
                })
                .style(muted),
                space().height(18),
                button(
                    text("Přihlásit pracovní / školní účet")
                        .center()
                        .width(Length::Fill)
                )
                .width(Length::Fill)
                .padding([11, 20])
                .style(style::primary)
                .on_press_maybe((!busy).then_some(Msg::StartBrowserLogin(AccountKind::Work))),
                button(
                    text("Přihlásit pracovní účet kódem")
                        .center()
                        .width(Length::Fill)
                )
                .width(Length::Fill)
                .padding([11, 20])
                .style(style::secondary)
                .on_press_maybe((!busy).then_some(Msg::StartLogin(AccountKind::Work))),
                text("Kódem se lze přihlásit i z jiného zařízení; některé organizace ho blokují.")
                    .size(11)
                    .style(muted),
                button(
                    text("Přihlásit osobní účet (experimentální)")
                        .center()
                        .width(Length::Fill)
                )
                .width(Length::Fill)
                .padding([11, 20])
                .style(style::secondary)
                .on_press_maybe((!busy).then_some(Msg::StartLogin(AccountKind::Personal))),
            ]
            .spacing(10)
            .into(),
        };
        let mut col = column![content].spacing(10);
        if back.is_some() {
            col = col.push(space().height(4)).push(
                button(text("← Zpět").size(13))
                    .padding([6, 12])
                    .style(style::ghost)
                    .on_press(Msg::CancelLogin),
            );
        } else if code.is_none() && !self.accounts.is_empty() {
            // např. po vypršení přihlášení: lze pokračovat jiným účtem
            col = col
                .push(space().height(8))
                .push(text("Nebo pokračovat účtem").size(12).style(muted));
            for a in &self.accounts {
                col = col.push(account_item(
                    a,
                    false,
                    None,
                    Msg::SwitchAccount(a.id.clone()),
                ));
            }
        }
        centered(container(col).padding(32).max_width(440).style(style::card))
    }

    /// Krok přihlášení v prohlížeči: odkaz na stránku a pole pro adresu, kde prohlížeč skončil.
    fn browser_login_view(&self) -> Element<'_, Msg> {
        let Some(login) = &self.browser_login else {
            return space().into();
        };
        let mut col = column![
            text("Přihlášení v prohlížeči").size(22).font(BOLD),
            text("1. V prohlížeči se otevřela přihlašovací stránka Microsoftu.").style(muted),
            row![
                button(text("Otevřít znovu").size(12))
                    .padding([4, 10])
                    .style(style::secondary)
                    .on_press(Msg::OpenUrl(login.url.clone())),
                copy_button(&login.url, "Odkaz zkopírován"),
            ]
            .spacing(8),
        ]
        .spacing(10);
        if login.uses_loopback() {
            col = col.push(
                text("2. Přihlaste se. Okno pak pokračuje samo.")
                    .size(13)
                    .style(muted),
            );
        } else {
            col = col
                .push(
                    text(
                        "2. Po přihlášení zkopírujte adresu stránky, na které prohlížeč \
                         skončí (Ctrl+L, Ctrl+C), a vraťte se sem. Tymy ji převezme ze \
                         schránky samo.",
                    )
                    .size(13)
                    .style(muted),
                )
                .push(
                    text_input("Nebo adresu vložte sem", &self.login_input)
                        .on_input(Msg::BrowserLoginInput)
                        .padding([8, 12])
                        .size(13)
                        .style(style::search_input),
                );
        }
        col = col.push(
            text(if self.redeeming {
                "Ověřuji přihlášení…".to_owned()
            } else {
                self.toast.clone().unwrap_or_default()
            })
            .size(12)
            .style(style::accent),
        );
        col.into()
    }

    /// Rozbalené menu účtů pod hlavičkou panelu (prázdné, když je zavřené).
    fn account_menu_view<'a>(&'a self, session: &'a Session) -> Element<'a, Msg> {
        if !self.account_menu {
            return space().height(0).into();
        }
        let active = &session.profile.account_id;
        let mut col = Column::new().spacing(2);
        for a in &self.accounts {
            let is_active = &a.id == active;
            let status = if is_active {
                AccountStatus::Unread(self.conversations.iter().filter(|c| self.unread(c)).count())
            } else {
                match self.others.get(&a.id) {
                    Some(OtherAccount {
                        unread: Some(n), ..
                    }) => AccountStatus::Unread(*n),
                    Some(OtherAccount { error: Some(e), .. }) => AccountStatus::Note(e.clone()),
                    _ => AccountStatus::Note("zjišťuji…".into()),
                }
            };
            col = col.push(account_item(
                a,
                is_active,
                Some(status),
                Msg::SwitchAccount(a.id.clone()),
            ));
        }
        col = col.push(
            button(text("＋  Přidat účet").size(13))
                .padding([7, 10])
                .width(Length::Fill)
                .style(|t: &Theme, st| style::list_item(t, st, false))
                .on_press(Msg::AddAccount),
        );
        col = col.push(
            button(
                row![
                    text("Spouštět po přihlášení").size(13).width(Length::Fill),
                    text(if self.autostart { "✓" } else { "" })
                        .size(13)
                        .style(style::accent),
                ]
                .align_y(iced::Alignment::Center),
            )
            .padding([7, 10])
            .width(Length::Fill)
            .style(|t: &Theme, st| style::list_item(t, st, false))
            .on_press(Msg::ToggleAutostart),
        );
        if self.confirm_sign_out {
            let name = session.profile.display_name.as_deref().unwrap_or("účet");
            col = col.push(
                column![
                    text(format!(
                        "Odhlásit {name}? Z počítače se smaže přihlášení i stažené zprávy tohoto účtu."
                    ))
                    .size(12),
                    row![
                        button(text("Odhlásit").size(12).font(SEMIBOLD))
                            .padding([3, 10])
                            .style(style::danger_button)
                            .on_press(Msg::SignOut),
                        button(text("Zrušit").size(12))
                            .padding([3, 10])
                            .style(style::ghost)
                            .on_press(Msg::CancelSignOut),
                    ]
                    .spacing(4),
                ]
                .spacing(6)
                .padding([6, 10]),
            );
        } else {
            col = col.push(
                button(text("Odhlásit tento účet").size(13).style(style::danger))
                    .padding([7, 10])
                    .width(Length::Fill)
                    .style(|t: &Theme, st| style::list_item(t, st, false))
                    .on_press(Msg::AskSignOut),
            );
        }
        container(container(col).padding(6).style(style::card))
            .padding(iced::Padding {
                top: 0.0,
                right: 12.0,
                bottom: 10.0,
                left: 12.0,
            })
            .into()
    }

    fn sidebar<'a>(&'a self, session: &'a Session) -> Element<'a, Msg> {
        let me = session.profile.display_name.as_deref().unwrap_or("Já");
        let query = self.search.trim().to_lowercase();
        let unread_in = |pred: fn(&Conversation) -> bool| {
            self.conversations
                .iter()
                .filter(|c| pred(c) && self.unread(c))
                .count()
        };
        let (theme_icon, theme_tip) = match self.theme_mode {
            ThemeMode::System => ("◐", "Vzhled: podle systému (klikněte pro světlý)"),
            ThemeMode::Light => ("☀", "Vzhled: světlý (klikněte pro tmavý)"),
            ThemeMode::Dark => ("☾", "Vzhled: tmavý (klikněte pro systémový)"),
        };
        let who = button(
            row![
                avatar(me, 34.0),
                column![
                    text(truncate(me, 20)).size(14).font(SEMIBOLD),
                    text(truncate(
                        session.profile.username.as_deref().unwrap_or_default(),
                        26
                    ))
                    .size(11)
                    .style(muted),
                ]
                .spacing(1)
                .width(Length::Fill),
                match self.others_unread() {
                    0 => Element::from(space().width(0)),
                    n => iced::widget::tooltip(
                        badge(n),
                        container(text("Nepřečtené v ostatních účtech").size(11))
                            .padding([3, 8])
                            .style(style::tooltip),
                        iced::widget::tooltip::Position::Bottom,
                    )
                    .into(),
                },
                text(if self.account_menu { "▴" } else { "▾" })
                    .size(12)
                    .style(muted),
            ]
            .spacing(10)
            .align_y(iced::Alignment::Center),
        )
        .padding([4, 6])
        .width(Length::Fill)
        .style(move |t: &Theme, st| style::list_item(t, st, self.account_menu))
        .on_press(Msg::ToggleAccountMenu);
        let profile = row![
            iced::widget::tooltip(
                who,
                container(text("Účty").size(11))
                    .padding([3, 8])
                    .style(style::tooltip),
                iced::widget::tooltip::Position::Bottom,
            ),
            icon_button(theme_icon, Some(Msg::ToggleTheme), theme_tip),
        ]
        .spacing(6)
        .align_y(iced::Alignment::Center);
        let tabs = container(
            column![
                row![
                    tab_button(
                        "Chaty",
                        unread_in(Conversation::is_chat),
                        self.tab == Tab::Chats,
                        Tab::Chats
                    ),
                    tab_button(
                        "Týmy",
                        unread_in(|c| c.kind == ConversationKind::Channel),
                        self.tab == Tab::Teams,
                        Tab::Teams
                    ),
                ]
                .spacing(2),
                tab_button("Kalendář", 0, self.tab == Tab::Calendar, Tab::Calendar),
            ]
            .spacing(2),
        )
        .padding(3)
        .style(style::segmented);
        let (items, placeholder) = match self.tab {
            Tab::Chats => (self.chat_list(session, &query), "Hledat chat nebo osobu…"),
            Tab::Teams => (self.team_list(&query), "Hledat tým nebo kanál…"),
            Tab::Calendar => (self.calendar_list(&query), "Hledat v kalendáři…"),
        };
        container(column![
            container(profile).padding(iced::Padding {
                top: 10.0,
                right: 12.0,
                bottom: 8.0,
                left: 8.0
            }),
            self.account_menu_view(session),
            container(tabs).padding([0, 12]),
            container({
                let search = text_input(placeholder, &self.search)
                    .on_input(Msg::Search)
                    .padding([8, 12])
                    .size(13)
                    .style(style::search_input);
                let plus = match self.tab {
                    Tab::Chats => Some((Msg::NewGroup, "Nový skupinový chat")),
                    // nová schůzka je jen vpravo nahoře nad týdenní mřížkou
                    Tab::Calendar | Tab::Teams => None,
                };
                let new_group = plus.map(|(msg, tip)| {
                    iced::widget::tooltip(
                        button(text("＋").size(18).center().width(Length::Fill))
                            .width(34)
                            .padding([2, 0])
                            .style(style::ghost)
                            .on_press(msg),
                        container(text(tip).size(11))
                            .padding([3, 8])
                            .style(style::tooltip),
                        iced::widget::tooltip::Position::Bottom,
                    )
                });
                row![search]
                    .push(new_group)
                    .spacing(6)
                    .align_y(iced::Alignment::Center)
            })
            .padding([10, 12]),
            scrollable(
                Column::with_children(items)
                    .spacing(2)
                    .padding(iced::Padding {
                        top: 0.0,
                        right: 10.0,
                        bottom: 10.0,
                        left: 8.0
                    })
            )
            .direction(style::scrollbar())
            .style(style::thin_scrollbar)
            .on_scroll(Msg::ChatListScrolled)
            .height(Length::Fill),
        ])
        .width(SIDEBAR_WIDTH)
        .height(Length::Fill)
        .style(style::sidebar)
        .into()
    }

    /// Záložka Kalendář: agenda aktivního účtu po dnech (dnes + `cal::DAYS_AHEAD`).
    fn calendar_list<'a>(&'a self, query: &str) -> Vec<Element<'a, Msg>> {
        let mut items: Vec<Element<'a, Msg>> = vec![];
        let Some(account) = self.active_account() else {
            return items;
        };
        if self.calendar.errors.contains_key(account) {
            items.push(note("Kalendář se nepodařilo načíst, zkusím to znovu."));
        }
        let Some(events) = self.active_events() else {
            items.push(note(if self.calendar.loading.contains(account) {
                "Načítám kalendář…"
            } else {
                "Kalendář zatím není načtený"
            }));
            return items;
        };
        let now = Utc::now();
        let today = now.with_timezone(&Local).date_naive();
        let matches = |e: &&CalendarEvent| {
            query.is_empty()
                || [Some(&e.subject), e.location.as_ref(), e.organizer.as_ref()]
                    .into_iter()
                    .flatten()
                    .any(|s| s.to_lowercase().contains(query))
        };
        // při psaní nové schůzky není v agendě nic vybrané
        let selected = self
            .selected_event()
            .filter(|_| self.meeting_form.is_none())
            .map(|e| e.id.as_str());
        let mut day = None;
        let mut any = false;
        for e in events.iter().filter(matches) {
            // vícedenní události, které začaly dřív, patří pod dnešek
            let d = cal::start_day(e).max(today);
            if day != Some(d) {
                items.push(section_title(cal::day_label(d, today).to_uppercase()));
                day = Some(d);
            }
            items.push(event_item(e, now, selected == Some(e.id.as_str())));
            any = true;
        }
        if !any {
            items.push(note(if query.is_empty() {
                "Žádné události v příštích 14 dnech"
            } else {
                "Nic nenalezeno"
            }));
        }
        items
    }

    /// Detail události (místo vlákna zpráv).
    /// Záložka Kalendář vpravo: formulář nové schůzky, jinak týdenní mřížka a nad ní
    /// karta vybrané události.
    fn calendar_pane(&self) -> Element<'_, Msg> {
        if let Some(f) = &self.meeting_form {
            return meeting_form_view(f);
        }
        let week = self.week_view();
        let Some(e) = self.selected_event() else {
            return week;
        };
        let dim = mouse_area(
            container(space())
                .width(Length::Fill)
                .height(Length::Fill)
                .style(|_: &Theme| container::Style {
                    background: Some(Color::from_rgba(0.0, 0.0, 0.0, 0.35).into()),
                    ..Default::default()
                }),
        )
        .on_press(Msg::CloseEvent);
        // klik do karty nesmí projít na ztmavené pozadí (to kartu zavře)
        let card = mouse_area(
            container(self.event_detail(e))
                .max_width(620)
                .style(style::card),
        )
        .on_press(Msg::Noop);
        stack![
            week,
            dim,
            container(card)
                .center_x(Length::Fill)
                .center_y(Length::Fill)
                .padding(24),
        ]
        .into()
    }

    /// Detail události (obsah karty nad týdenní mřížkou).
    fn event_detail<'a>(&'a self, e: &'a CalendarEvent) -> Element<'a, Msg> {
        let now = Utc::now();
        let today = now.with_timezone(&Local).date_naive();
        let day = cal::start_day(e);
        let header = container(
            row![
                date_badge(day),
                column![
                    text(e.subject.clone()).size(16).font(BOLD),
                    text(format!(
                        "{} · {}",
                        cal::day_label(day, today),
                        cal::time_label(e)
                    ))
                    .size(12)
                    .style(muted),
                ]
                .spacing(1)
                .width(Length::Fill),
                icon_button("✕", Some(Msg::CloseEvent), "Zavřít (Esc)"),
            ]
            .spacing(12)
            .align_y(iced::Alignment::Center),
        )
        .padding([12, 20])
        .width(Length::Fill);

        let mut body = Column::new().spacing(14).padding([4, 24]);
        let mut status: Vec<Element<'_, Msg>> = vec![];
        if e.cancelled {
            status.push(
                text("Událost byla zrušena")
                    .size(13)
                    .font(SEMIBOLD)
                    .style(style::danger)
                    .into(),
            );
        } else if e.start <= now && now < e.end {
            status.push(text("Probíhá").size(13).font(SEMIBOLD).style(accent).into());
        } else if e.end <= now {
            status.push(text("Skončila").size(13).style(muted).into());
        }
        if let Some(r) = cal::response_label(e) {
            status.push(text(r).size(13).style(muted).into());
        }
        if !status.is_empty() {
            body = body.push(row(status).spacing(14));
        }
        let field = |label: &'static str, value: &str| {
            row![
                text(label).size(13).style(muted).width(100),
                text(value.to_owned()).size(13),
            ]
            .spacing(10)
        };
        if let Some(l) = &e.location {
            body = body.push(field("Místo", l));
        }
        if let Some(o) = &e.organizer {
            body = body.push(field("Organizátor", o));
        }
        let mut actions = row![].spacing(10);
        if let Some(url) = e.join_url.as_ref().filter(|_| !e.cancelled && e.end > now) {
            actions = actions.push(
                button(text("Připojit").size(13).font(SEMIBOLD))
                    .padding([7, 16])
                    .style(style::primary)
                    .on_press(Msg::JoinMeeting(e.id.clone(), url.clone())),
            );
        }
        if let Some(chat) = e
            .chat
            .as_ref()
            .filter(|id| self.conversations.iter().any(|c| &c.id == *id))
        {
            actions = actions.push(
                button(text("Chat schůzky").size(13))
                    .padding([7, 16])
                    .style(style::secondary)
                    .on_press(Msg::OpenMeetingChat(chat.clone())),
            );
        }
        body = body.push(actions);
        let upcoming = !e.cancelled && e.end > now;
        let busy = self.event_busy;
        if upcoming && e.is_organizer {
            let cancel: Element<'_, Msg> = if self.confirm_cancel_event.as_deref() == Some(&e.id) {
                row![
                    text("Zrušit schůzku? Účastníci dostanou zrušení.")
                        .size(13)
                        .style(style::danger),
                    button(text("Zrušit schůzku").size(13).font(SEMIBOLD))
                        .padding([6, 14])
                        .style(style::danger_button)
                        .on_press_maybe((!busy).then(|| Msg::CancelEvent(e.id.clone()))),
                    button(text("Ponechat").size(13))
                        .padding([6, 14])
                        .style(style::ghost)
                        .on_press(Msg::KeepEvent),
                ]
                .spacing(10)
                .align_y(iced::Alignment::Center)
                .into()
            } else {
                button(text("Zrušit schůzku…").size(13))
                    .padding([6, 14])
                    .style(style::secondary)
                    .on_press(Msg::AskCancelEvent(e.id.clone()))
                    .into()
            };
            body = body.push(cancel);
        } else if upcoming && e.response != EventResponse::None {
            let reply = |label: &'static str, r: EventReply, current: bool| {
                button(
                    text(label)
                        .size(13)
                        .font(if current { SEMIBOLD } else { Font::DEFAULT }),
                )
                .padding([6, 14])
                .style(move |t: &Theme, st| style::segment(t, st, current))
                .on_press_maybe((!busy).then(|| Msg::ReplyEvent(e.id.clone(), r)))
            };
            body = body.push(
                row![
                    reply(
                        "Přijmout",
                        EventReply::Accept,
                        e.response == EventResponse::Accepted
                    ),
                    reply(
                        "Předběžně",
                        EventReply::Tentative,
                        e.response == EventResponse::Tentative
                    ),
                    reply(
                        "Odmítnout",
                        EventReply::Decline,
                        e.response == EventResponse::Declined
                    ),
                ]
                .spacing(6),
            );
        }
        if let Some(p) = &e.preview {
            body = body.push(
                container(text(p.clone()).size(13))
                    .padding([12, 14])
                    .width(Length::Fill)
                    .style(style::card),
            );
        }
        let reminder = if !self.calendar.settings.reminders {
            "Připomínky jsou vypnuté (settings.toml → [calendar] reminders).".to_owned()
        } else if e.all_day {
            "Připomenu v 8:00 v den události.".to_owned()
        } else {
            let m = e
                .reminder_minutes
                .unwrap_or(i64::from(self.calendar.settings.reminder_minutes));
            format!("Připomenu {m} min před začátkem.")
        };
        body = body.push(text(reminder).size(12).style(muted));
        column![header, body, space().height(16)].into()
    }

    /// Týdenní mřížka: hlavička s přepínáním týdnů, dny, celodenní události, hodiny s bloky.
    fn week_view(&self) -> Element<'_, Msg> {
        let now = Utc::now();
        let today = now.with_timezone(&Local).date_naive();
        let monday = self.week;
        let events = self.week_events();
        let (all_day, blocks) = cal::week_layout(&events, monday);
        let selected = self.calendar.selected.as_deref();
        let pane = self.window_width - SIDEBAR_WIDTH - 1.0;
        let col_w = ((pane - 2.0 * WEEK_PAD - WEEK_SCROLLBAR - WEEK_GUTTER) / 7.0 - 1.0).max(40.0);
        let loading = self
            .active_account()
            .is_some_and(|a| self.week_loading.contains(&(a.clone(), monday)));

        let nav = |label: &'static str, tip: &'static str, delta: i64| {
            iced::widget::tooltip(
                button(text(label).size(13))
                    .padding([5, 10])
                    .style(style::secondary)
                    .on_press(Msg::WeekShift(delta)),
                container(text(tip).size(11))
                    .padding([3, 8])
                    .style(style::tooltip),
                iced::widget::tooltip::Position::Bottom,
            )
        };
        let header = container(
            row![
                nav("‹", "Předchozí týden", -1),
                nav("Dnes", "Tento týden", 0),
                nav("›", "Další týden", 1),
                text(cal::week_label(monday)).size(16).font(BOLD),
                text(if loading { "Načítám…" } else { "" })
                    .size(12)
                    .style(muted),
                space::horizontal(),
                button(text("＋ Nová schůzka").size(13))
                    .padding([6, 12])
                    .style(style::primary)
                    .on_press(Msg::NewMeeting),
            ]
            .spacing(8)
            .align_y(iced::Alignment::Center),
        )
        .padding([10, 16])
        .width(Length::Fill)
        .style(style::header);

        let day_of = |d: usize| monday + chrono::Duration::days(d as i64);
        let mut days = row![space().width(WEEK_GUTTER)];
        for d in 0..7 {
            let date = day_of(d);
            let is_today = date == today;
            // v hlavičce jen mezera (svislá čára s výškou Fill by roztáhla řádek)
            days = days.push(space().width(1)).push(
                container(
                    column![
                        text(cal::weekday_short(d)).size(11).style(if is_today {
                            accent
                        } else {
                            muted
                        }),
                        text(date.day().to_string())
                            .size(17)
                            .font(if is_today { BOLD } else { Font::DEFAULT })
                            .style(move |t: &Theme| if is_today {
                                accent(t)
                            } else {
                                text::Style::default()
                            }),
                    ]
                    .align_x(iced::Alignment::Center),
                )
                .width(col_w)
                .padding([6, 0])
                .center_x(col_w),
            );
        }

        let mut top = column![header, container(days).padding([0, WEEK_PAD as u16])];
        if !all_day.is_empty() {
            let mut cells = row![
                container(text("celý den").size(10).style(muted))
                    .width(WEEK_GUTTER)
                    .padding([4, 4])
            ];
            for d in 0..7 {
                let chips = all_day
                    .iter()
                    .filter(|(_, first, last)| *first <= d && d <= *last)
                    .map(|(i, _, _)| {
                        let e = &events[*i];
                        event_block(
                            e,
                            col_w - 4.0,
                            22.0,
                            selected == Some(e.id.as_str()),
                            now,
                            true,
                        )
                    });
                cells = cells.push(space().width(1)).push(
                    container(Column::with_children(chips).spacing(2))
                        .width(col_w)
                        .padding([2, 2]),
                );
            }
            top = top.push(container(cells).padding([0, WEEK_PAD as u16]));
        }
        top = top.push(hline());

        let hours = Column::with_children((0..24).map(|h| {
            container(text(format!("{h}:00")).size(10).style(muted))
                .height(WEEK_HOUR)
                .width(WEEK_GUTTER)
                .padding(iced::Padding {
                    top: 0.0,
                    right: 6.0,
                    bottom: 0.0,
                    left: 0.0,
                })
                .align_right(WEEK_GUTTER)
                .into()
        }));
        let mut grid = row![hours];
        for d in 0..7 {
            let is_today = day_of(d) == today;
            let lines = Column::with_children(
                (0..24).map(|_| column![hline(), space().height(WEEK_HOUR - 1.0)].into()),
            );
            let base = container(lines)
                .width(col_w)
                .height(WEEK_HOUR * 24.0)
                .style(move |t: &Theme| container::Style {
                    background: is_today.then(|| {
                        let c = style::colors(t);
                        Color {
                            a: 0.35,
                            ..c.primary_soft
                        }
                        .into()
                    }),
                    ..Default::default()
                });
            let mut layers: Vec<Element<'_, Msg>> = vec![base.into()];
            for b in blocks.iter().filter(|b| b.day == d) {
                let e = &events[b.event];
                let y = b.from as f32 / 60.0 * WEEK_HOUR;
                let h = ((b.to - b.from) as f32 / 60.0 * WEEK_HOUR - 2.0).max(14.0);
                let lane_w = (col_w - 4.0) / b.lanes as f32;
                let x = 2.0 + lane_w * b.lane as f32;
                layers.push(
                    container(event_block(
                        e,
                        lane_w - 2.0,
                        h,
                        selected == Some(e.id.as_str()),
                        now,
                        false,
                    ))
                    .padding(iced::Padding {
                        top: y + 1.0,
                        right: 0.0,
                        bottom: 0.0,
                        left: x,
                    })
                    .into(),
                );
            }
            if is_today {
                let local = now.with_timezone(&Local);
                let y = (local.hour() * 60 + local.minute()) as f32 / 60.0 * WEEK_HOUR;
                layers.push(
                    container(container(space()).width(Length::Fill).height(2).style(
                        |t: &Theme| container::Style {
                            background: Some(style::colors(t).danger.into()),
                            ..Default::default()
                        },
                    ))
                    .padding(iced::Padding {
                        top: y,
                        right: 0.0,
                        bottom: 0.0,
                        left: 0.0,
                    })
                    .into(),
                );
            }
            if let Some(drag) = self.grid_drag.filter(|g| g.day == d) {
                let (from, to) = cal::drag_range(drag.anchor, drag.current);
                let label = format!(
                    "{}–{}",
                    cal::minutes_time(from).format("%H:%M"),
                    cal::minutes_time(to).format("%H:%M")
                );
                layers.push(
                    container(
                        container(text(label).size(11).font(SEMIBOLD))
                            .padding([2, 6])
                            .width(col_w - 4.0)
                            .height((to - from) as f32 / 60.0 * WEEK_HOUR)
                            .style(|t: &Theme| {
                                let c = style::colors(t);
                                container::Style {
                                    background: Some(
                                        Color {
                                            a: 0.35,
                                            ..c.primary
                                        }
                                        .into(),
                                    ),
                                    text_color: Some(c.text),
                                    border: Border {
                                        color: c.primary,
                                        width: 1.0,
                                        radius: 5.0.into(),
                                    },
                                    ..Default::default()
                                }
                            }),
                    )
                    .padding(iced::Padding {
                        top: from as f32 / 60.0 * WEEK_HOUR,
                        right: 0.0,
                        bottom: 0.0,
                        left: 2.0,
                    })
                    .into(),
                );
            }
            // tažením v prázdném místě dne se vybere čas nové schůzky (klik na schůzku
            // zachytí její tlačítko dřív)
            let day_area = mouse_area(iced::widget::Stack::with_children(layers))
                .on_move(move |p| Msg::GridMove(d, p.y))
                .on_press(Msg::GridPress(d))
                .interaction(iced::mouse::Interaction::Crosshair);
            grid = grid.push(vline()).push(day_area);
        }
        let body = scrollable(container(grid).padding(iced::Padding {
            top: 6.0,
            right: WEEK_PAD,
            bottom: 12.0,
            left: WEEK_PAD,
        }))
        .id(WEEK_GRID)
        .direction(style::scrollbar())
        .style(style::thin_scrollbar)
        .height(Length::Fill);
        let mut content = top.push(body);
        if events.is_empty() && !loading {
            content = content.push(space().height(0));
        }
        content.into()
    }

    /// Záložka Chaty: připnuté v ručním pořadí, ostatní podle poslední komunikace, pak lidé.
    fn chat_list<'a>(&'a self, session: &'a Session, query: &str) -> Vec<Element<'a, Msg>> {
        let matches = |c: &&Conversation| {
            c.is_chat()
                && self.listed(c)
                && (query.is_empty() || self.peers.title(c).to_lowercase().contains(query))
        };
        let (pinned, rest) = order_chats(self.conversations.iter().filter(matches), &self.pins);
        let mut items: Vec<Element<'a, Msg>> = vec![];
        if !pinned.is_empty() {
            items.push(section_label("PŘIPNUTÉ"));
            let n = pinned.len();
            for (i, c) in pinned.into_iter().enumerate() {
                items.push(self.chat_row(session, c, Some((i, n))));
            }
            if !rest.is_empty() {
                items.push(section_label("OSTATNÍ"));
            }
        }
        items.extend(rest.into_iter().map(|c| self.chat_row(session, c, None)));
        if let Some(more) = self.more_chats_item() {
            items.push(more);
        }

        if query.chars().count() >= MIN_PEOPLE_QUERY {
            items.push(section_label("LIDÉ"));
            if self.searching_people {
                items.push(note("Hledám…"));
            } else if self.people.is_empty() {
                items.push(note("Nikdo nenalezen"));
            }
            items.extend(self.people.iter().map(person_item));
            if let Some(n) = &self.people_note {
                items.push(note(n));
            } else if !looks_like_email(&self.search) {
                items.push(note(
                    "Tip: celou e-mailovou adresou najdete i lidi mimo organizaci.",
                ));
            }
        }
        if items.is_empty() {
            items.push(note("Žádné chaty"));
        }
        items.extend(self.hit_items(query));
        items
    }

    /// Konec načtené části seznamu: stav dočítání, nebo tlačítko pro další stránku.
    fn more_chats_item<'a>(&self) -> Option<Element<'a, Msg>> {
        self.chats_older.as_ref()?;
        Some(if self.loading_chats {
            note("Načítám starší chaty…")
        } else {
            container(
                button(text("Načíst starší chaty").size(12).style(muted))
                    .padding([4, 10])
                    .style(style::ghost)
                    .on_press(Msg::LoadOlderChats),
            )
            .width(Length::Fill)
            .center_x(Length::Fill)
            .padding([6, 0])
            .into()
        })
    }

    /// Sekce „Zprávy“ ve výsledcích hledání (fulltext v cache) a stav indexace.
    fn hit_items<'a>(&'a self, query: &str) -> Vec<Element<'a, Msg>> {
        if query.chars().count() < MIN_PEOPLE_QUERY {
            return vec![];
        }
        let c = style::colors(&self.theme());
        let mut items = vec![section_label("ZPRÁVY")];
        if self.message_hits.is_empty() {
            items.push(note("Žádné zprávy"));
        }
        items.extend(self.message_hits.iter().map(|h| self.hit_row(h, &c)));
        if self.index_running {
            let done = self.index_total.saturating_sub(self.index_queue.len());
            items.push(note(&format!(
                "Procházím starší historii ({done} z {} konverzací)…",
                self.index_total
            )));
        }
        items
    }

    fn hit_row<'a>(&'a self, h: &'a SearchHit, c: &style::Colors) -> Element<'a, Msg> {
        let m = &h.message;
        let title = self
            .conversations
            .iter()
            .find(|c| c.id == m.conversation_id)
            .map(|c| match c.kind {
                ConversationKind::Channel => format!("# {}", channel_name(c)),
                _ => self.peers.title(c),
            })
            .unwrap_or_else(|| "Konverzace".into());
        let mut spans: Vec<iced::widget::text::Span<'a, ()>> = vec![
            span(format!("{}: ", m.sender_name.as_deref().unwrap_or("?")))
                .font(SEMIBOLD)
                .color(c.muted),
        ];
        for (i, part) in h
            .snippet
            .replace('\n', " ")
            .split([HIT_START, HIT_END])
            .enumerate()
        {
            // liché úseky jsou mezi značkami = nalezená slova
            let sp = span(part.to_owned());
            spans.push(if i % 2 == 1 {
                sp.font(BOLD).color(c.primary)
            } else {
                sp.color(c.muted)
            });
        }
        button(
            column![
                row![
                    text(truncate(&title, 30))
                        .size(13)
                        .font(SEMIBOLD)
                        .wrapping(Wrapping::None),
                    space::horizontal(),
                    text(short_time(m.arrived_at.or(m.composed_at)))
                        .size(11)
                        .style(muted),
                ]
                .align_y(iced::Alignment::Center),
                rich_text(spans).size(12).line_height(1.3),
            ]
            .spacing(2),
        )
        .width(Length::Fill)
        .padding([7, 8])
        .style(|t: &Theme, status| style::list_item(t, status, false))
        .on_press(Msg::OpenHit(m.conversation_id.clone(), m.id.clone()))
        .into()
    }

    /// Záložka Týmy: týmy podle názvu, pod nimi kanály.
    /// Zobrazit konverzaci v seznamu? Nepřístupné ne (kromě právě otevřené).
    fn listed(&self, c: &Conversation) -> bool {
        if self.selected.as_ref() == Some(&c.id) {
            return true;
        }
        // bez přístupu (403) a prázdné chaty (bez zpráv, kromě rozepsaných nových) skrýt
        !self.forbidden.contains(&c.id) && !(is_empty_chat(c) && !self.is_draft(&c.id))
    }

    fn team_list<'a>(&'a self, query: &str) -> Vec<Element<'a, Msg>> {
        let mut items: Vec<Element<'a, Msg>> = vec![];
        let channels = self
            .conversations
            .iter()
            .filter(|c| c.kind == ConversationKind::Channel && self.listed(c));
        for g in group_teams(channels) {
            let team_match = g.name.to_lowercase().contains(query);
            let shown: Vec<_> = g
                .channels
                .iter()
                .filter(|c| {
                    query.is_empty() || team_match || channel_name(c).to_lowercase().contains(query)
                })
                .collect();
            if shown.is_empty() {
                continue;
            }
            // nepřečtené kanály týmu, i ty skryté hledáním
            let unread = g.channels.iter().filter(|c| self.unread(c)).count();
            let mut header = row![
                team_avatar(&g.name, 24.0),
                text(truncate(&g.name, 26)).size(13).font(SEMIBOLD),
            ]
            .spacing(8)
            .align_y(iced::Alignment::Center);
            if unread > 0 {
                header = header.push(space::horizontal()).push(badge(unread));
            }
            items.push(
                container(header)
                    .width(Length::Fill)
                    .padding(iced::Padding {
                        top: 12.0,
                        right: 10.0,
                        bottom: 4.0,
                        left: 8.0,
                    })
                    .into(),
            );
            items.extend(shown.into_iter().map(|c| self.channel_item(c)));
        }
        if let Some(more) = self.more_chats_item() {
            items.push(more);
        }
        if items.is_empty() {
            items.push(note(if query.is_empty() {
                "Nejste v žádném týmu"
            } else {
                "Nic nenalezeno"
            }));
        }
        items.extend(self.hit_items(query));
        items
    }

    fn channel_item<'a>(&'a self, c: &'a Conversation) -> Element<'a, Msg> {
        let selected = self.selected.as_ref() == Some(&c.id);
        let unread = self.unread(c);
        let font = if unread { BOLD } else { Font::DEFAULT };
        button(
            row![
                text("#").size(14).style(muted),
                text(truncate(&channel_name(c), 22))
                    .wrapping(Wrapping::None)
                    .size(13)
                    .font(font),
                space::horizontal(),
                text(short_time(c.last_activity))
                    .wrapping(Wrapping::None)
                    .size(11)
                    .style(muted),
                unread_dot(unread),
            ]
            .spacing(8)
            .align_y(iced::Alignment::Center),
        )
        .width(Length::Fill)
        .padding(iced::Padding {
            top: 7.0,
            right: 10.0,
            bottom: 7.0,
            left: 40.0,
        })
        .style(move |t: &Theme, status| style::list_item(t, status, selected))
        .on_press(Msg::Select(c.id.clone()))
        .into()
    }

    /// Řádek chatu s ovládáním připnutí (zobrazí se po najetí myší).
    /// `pin` = (pořadí, počet připnutých), pokud je chat připnutý.
    fn chat_row<'a>(
        &'a self,
        session: &Session,
        c: &'a Conversation,
        pin: Option<(usize, usize)>,
    ) -> Element<'a, Msg> {
        let hovered = self.hovered.as_ref() == Some(&c.id);
        let controls: Element<'a, Msg> = match pin {
            Some((i, n)) if hovered => column![
                icon_button(
                    "▲",
                    (i > 0).then(|| Msg::MovePin(c.id.clone(), -1)),
                    "Posunout nahoru"
                ),
                icon_button(
                    "▼",
                    (i + 1 < n).then(|| Msg::MovePin(c.id.clone(), 1)),
                    "Posunout dolů"
                ),
                icon_button("✕", Some(Msg::Unpin(c.id.clone())), "Odepnout"),
            ]
            .into(),
            Some(_) => container(text("📌").size(11).style(muted))
                .center_x(Length::Fill)
                .into(),
            None if hovered => icon_button("📌", Some(Msg::Pin(c.id.clone())), "Připnout nahoru"),
            None => space().into(),
        };
        mouse_area(
            row![
                self.chat_item(session, c),
                container(controls).width(26).center_x(26),
            ]
            .spacing(2)
            .align_y(iced::Alignment::Center),
        )
        .on_enter(Msg::Hover(c.id.clone()))
        .on_exit(Msg::Unhover(c.id.clone()))
        .into()
    }

    fn chat_item<'a>(&'a self, session: &Session, c: &'a Conversation) -> Element<'a, Msg> {
        let selected = self.selected.as_ref() == Some(&c.id);
        let unread = self.unread(c);
        let title = self.peers.title(c);
        let preview = c
            .last_message
            .as_ref()
            .filter(|m| m.is_user_content())
            .map(|m| {
                let body = preview_text(&m.content);
                if is_me(&session.profile, m) {
                    format!("Vy: {body}")
                } else {
                    body
                }
            })
            .unwrap_or_else(|| {
                if self.is_draft(&c.id) {
                    "Nový chat".into()
                } else {
                    String::new()
                }
            });
        let draft = (!selected).then(|| self.draft_preview(&c.id)).flatten();
        let has_draft = draft.is_some();
        let preview = match draft {
            Some(d) => format!("Koncept: {d}"),
            None => preview,
        };
        let title_font = if unread { BOLD } else { SEMIBOLD };
        let top = row![
            text(truncate(&title, 20))
                .wrapping(Wrapping::None)
                .size(14)
                .font(title_font),
            space::horizontal(),
            text(short_time(c.last_activity))
                .wrapping(Wrapping::None)
                .size(11)
                .style(if unread { accent } else { muted }),
        ]
        .spacing(6)
        .align_y(iced::Alignment::Center);
        let bottom = row![
            text(truncate(&preview, 27))
                .wrapping(Wrapping::None)
                .size(12)
                .style(if has_draft { style::danger } else { muted }),
            space::horizontal(),
            unread_dot(unread),
        ]
        .align_y(iced::Alignment::Center);
        button(
            row![avatar(&title, 38.0), column![top, bottom].spacing(3)]
                .spacing(10)
                .align_y(iced::Alignment::Center),
        )
        .width(Length::Fill)
        .padding([8, 8])
        .style(move |t: &Theme, status| style::list_item(t, status, selected))
        .on_press(Msg::Select(c.id.clone()))
        .into()
    }

    /// Nastavení skupinového chatu (místo vlákna): název, členové, přidání, opuštění.
    fn chat_panel_view<'a>(&'a self, p: &'a ChatPanel, session: &'a Session) -> Element<'a, Msg> {
        let busy = p.busy;
        let roster = self.rosters.get(&p.conv);
        let mut members = Column::new().spacing(2);
        let me = session.profile.display_name.as_deref().unwrap_or("Já");
        members = members.push(
            row![avatar(me, 28.0), text(format!("{me} (vy)")).size(13),]
                .spacing(10)
                .align_y(iced::Alignment::Center)
                .padding([4, 8]),
        );
        match roster {
            None => members = members.push(note("Načítám členy…")),
            Some(list) => {
                for m in list {
                    members = members.push(
                        row![
                            avatar(&m.display_name, 28.0),
                            column![
                                text(m.display_name.clone()).size(13),
                                text(truncate(&m.subtitle(), 48)).size(11).style(muted),
                            ]
                            .spacing(1),
                            space::horizontal(),
                            iced::widget::tooltip(
                                if p.confirm_remove.as_ref() == Some(&m.mri) {
                                    button(text("Opravdu odebrat?").size(12).font(SEMIBOLD))
                                        .padding([3, 10])
                                        .style(style::danger_button)
                                } else {
                                    button(text("Odebrat").size(12))
                                        .padding([3, 10])
                                        .style(style::ghost)
                                }
                                .on_press_maybe((!busy).then(|| Msg::RemoveMember(m.mri.clone()))),
                                container(
                                    text("Odebrat z chatu (potvrďte druhým klikem)").size(11)
                                )
                                .padding([3, 8])
                                .style(style::tooltip),
                                iced::widget::tooltip::Position::Left,
                            ),
                        ]
                        .spacing(10)
                        .align_y(iced::Alignment::Center)
                        .padding([4, 8]),
                    );
                }
            }
        }

        let mut results = Column::new().spacing(2);
        if p.searching {
            results = results.push(note("Hledám…"));
        } else if p.query.trim().chars().count() >= MIN_PEOPLE_QUERY && p.results.is_empty() {
            results = results.push(note("Nikdo nenalezen"));
        }
        let me_mri = my_mri(&session.profile);
        let is_member = |mri: &tymy_core::UserId| {
            *mri == me_mri || roster.is_some_and(|r| r.iter().any(|x| x.mri == *mri))
        };
        for person in p.results.iter().filter(|x| !is_member(&x.mri)) {
            results = results.push(
                button(
                    row![
                        avatar(&person.display_name, 28.0),
                        column![
                            text(truncate(&person.display_name, 32))
                                .size(13)
                                .font(SEMIBOLD),
                            text(truncate(&person.subtitle(), 48)).size(11).style(muted),
                        ]
                        .spacing(1),
                        space::horizontal(),
                        text("Přidat").size(12).style(accent),
                    ]
                    .spacing(10)
                    .align_y(iced::Alignment::Center),
                )
                .width(Length::Fill)
                .padding([6, 8])
                .style(|t: &Theme, st| style::list_item(t, st, false))
                .on_press_maybe((!busy).then(|| Msg::AddMember(person.clone()))),
            );
        }

        let leave: Element<'a, Msg> = if p.confirm_leave {
            row![
                text("Opravdu opustit chat? Zpět se dostanete jen pozváním.")
                    .size(13)
                    .style(style::danger),
                button(text("Opustit").size(13).font(SEMIBOLD))
                    .padding([6, 14])
                    .style(style::danger_button)
                    .on_press_maybe((!busy).then_some(Msg::Leave)),
                button(text("Zrušit").size(13))
                    .padding([6, 14])
                    .style(style::ghost)
                    .on_press(Msg::CancelLeave),
            ]
            .spacing(10)
            .align_y(iced::Alignment::Center)
            .into()
        } else {
            button(text("Opustit chat").size(13))
                .padding([6, 14])
                .style(style::secondary)
                .on_press(Msg::AskLeave)
                .into()
        };

        let body = column![
            text("Název chatu").size(12).style(muted),
            row![
                text_input("Bez názvu (podle členů)", &p.topic)
                    .on_input(Msg::PanelTopic)
                    .on_submit(Msg::SaveTopic)
                    .padding([8, 12])
                    .size(13)
                    .style(style::search_input),
                button(text("Uložit").size(13).font(SEMIBOLD))
                    .padding([7, 16])
                    .style(style::primary)
                    .on_press_maybe((!busy).then_some(Msg::SaveTopic)),
            ]
            .spacing(8)
            .align_y(iced::Alignment::Center),
            space().height(8),
            text(match roster {
                Some(r) => format!("Členové ({})", r.len() + 1),
                None => "Členové".into(),
            })
            .size(12)
            .style(muted),
            members,
            space().height(8),
            text("Přidat lidi").size(12).style(muted),
            text_input("Jméno nebo e-mail…", &p.query)
                .on_input(Msg::PanelQuery)
                .padding([8, 12])
                .size(13)
                .style(style::search_input),
            results,
            space().height(12),
            leave,
        ]
        .spacing(6)
        .padding([16, 24])
        .max_width(640);
        scrollable(body)
            .direction(style::scrollbar())
            .style(style::thin_scrollbar)
            .height(Length::Fill)
            .into()
    }

    /// Výběr lidí a názvu pro nový skupinový chat (místo vlákna).
    fn group_builder_view<'a>(&'a self, g: &'a GroupBuilder) -> Element<'a, Msg> {
        let header = container(
            column![
                text("Nový skupinový chat").size(16).font(BOLD),
                text("Chat vznikne první odeslanou zprávou.")
                    .size(12)
                    .style(muted),
            ]
            .spacing(1),
        )
        .padding([12, 20])
        .width(Length::Fill)
        .style(style::header);

        let chips = iced::widget::Row::with_children(g.members.iter().map(|p| {
            button(
                row![
                    avatar(&p.display_name, 20.0),
                    text(truncate(&p.display_name, 24)).size(12),
                    text("✕").size(11).style(muted),
                ]
                .spacing(6)
                .align_y(iced::Alignment::Center),
            )
            .padding([3, 8])
            .style(style::secondary)
            .on_press(Msg::GroupRemove(p.mri.clone()))
            .into()
        }))
        .spacing(6)
        .wrap();

        let mut results = Column::new().spacing(2);
        if g.searching {
            results = results.push(note("Hledám…"));
        } else if g.query.trim().chars().count() >= MIN_PEOPLE_QUERY && g.results.is_empty() {
            results = results.push(note("Nikdo nenalezen"));
        }
        for p in g
            .results
            .iter()
            .filter(|p| !g.members.iter().any(|m| m.mri == p.mri))
        {
            let subtitle = p.subtitle();
            results = results.push(
                button(
                    row![
                        avatar(&p.display_name, 30.0),
                        column![
                            text(truncate(&p.display_name, 32)).size(13).font(SEMIBOLD),
                            text(truncate(&subtitle, 48)).size(11).style(muted),
                        ]
                        .spacing(1),
                    ]
                    .spacing(10)
                    .align_y(iced::Alignment::Center),
                )
                .width(Length::Fill)
                .padding([6, 8])
                .style(|t: &Theme, st| style::list_item(t, st, false))
                .on_press(Msg::GroupAdd(p.clone())),
            );
        }

        let ready = g.members.len() >= 2;
        let body = column![
            text("Název (nepovinný)").size(12).style(muted),
            text_input("Např. Projekt Tymy", &g.topic)
                .on_input(Msg::GroupTopic)
                .padding([8, 12])
                .size(13)
                .style(style::search_input),
            space().height(6),
            text(format!("Lidé ({})", g.members.len()))
                .size(12)
                .style(muted),
            chips,
            text_input("Přidat člověka: jméno nebo e-mail…", &g.query)
                .id(GROUP_QUERY)
                .on_input(Msg::GroupQuery)
                .padding([8, 12])
                .size(13)
                .style(style::search_input),
            scrollable(results)
                .direction(style::scrollbar())
                .style(style::thin_scrollbar)
                .height(Length::Fill),
            text(if ready {
                ""
            } else {
                "Vyberte aspoň dva lidi. Chat s jedním člověkem otevřete hledáním vlevo."
            })
            .size(12)
            .style(muted),
            row![
                button(text("Zrušit").size(13))
                    .padding([7, 14])
                    .style(style::ghost)
                    .on_press(Msg::GroupCancel),
                button(text("Pokračovat").size(13).font(SEMIBOLD))
                    .padding([7, 16])
                    .style(style::primary)
                    .on_press_maybe(ready.then_some(Msg::GroupCreate)),
            ]
            .spacing(8),
        ]
        .spacing(8)
        .max_width(560)
        .padding([16, 24]);
        column![header, body].into()
    }

    fn chat_pane<'a>(&'a self, session: &'a Session) -> Element<'a, Msg> {
        if let Some(g) = &self.group_builder {
            return self.group_builder_view(g);
        }
        let Some(conv) = self
            .selected
            .as_ref()
            .and_then(|id| self.conversations.iter().find(|c| &c.id == id))
        else {
            return centered(
                column![logo(56.0), text("Vyberte chat vlevo").size(15).style(muted),]
                    .spacing(14)
                    .align_x(iced::Alignment::Center),
            );
        };
        let title = self.conv_title(conv, false);
        let subtitle = match conv.kind {
            ConversationKind::OneOnOne if self.is_draft(&conv.id) => "Nový chat".to_owned(),
            ConversationKind::OneOnOne => "Chat".to_owned(),
            ConversationKind::Group => "Skupinový chat".to_owned(),
            ConversationKind::Meeting => "Chat schůzky".to_owned(),
            ConversationKind::Channel => match self.team_name(conv) {
                Some(team) => format!("Kanál týmu {team}"),
                None => "Kanál týmu".to_owned(),
            },
            _ => String::new(),
        };
        let header_avatar = if conv.kind == ConversationKind::Channel {
            team_avatar(&self.team_name(conv).unwrap_or_else(|| title.clone()), 38.0)
        } else {
            avatar(&title, 38.0)
        };
        let manage: Element<'a, Msg> =
            if conv.kind == ConversationKind::Group && !self.is_draft(&conv.id) {
                let open = self.chat_panel.is_some();
                iced::widget::tooltip(
                    button(
                        text(if open {
                            "Zpět k chatu"
                        } else {
                            "⚙ Členové"
                        })
                        .size(13),
                    )
                    .padding([6, 12])
                    .style(style::secondary)
                    .on_press(Msg::ToggleChatPanel),
                    container(text("Název, členové, opuštění chatu").size(11))
                        .padding([3, 8])
                        .style(style::tooltip),
                    iced::widget::tooltip::Position::Bottom,
                )
                .into()
            } else {
                space().into()
            };
        let header = container(
            row![
                header_avatar,
                column![
                    text(title.clone()).size(16).font(BOLD),
                    text(subtitle).size(12).style(muted),
                ]
                .spacing(1),
                space::horizontal(),
                manage,
            ]
            .spacing(12)
            .align_y(iced::Alignment::Center),
        )
        .padding([12, 20])
        .width(Length::Fill)
        .style(style::header);

        if let Some(p) = self.chat_panel.as_ref().filter(|p| p.conv == conv.id) {
            return column![header, hline(), self.chat_panel_view(p, session)].into();
        }

        if self.forbidden.contains(&conv.id) {
            let notice = centered(
                column![
                    text("K této konverzaci nemáte přístup")
                        .size(15)
                        .font(SEMIBOLD),
                    text(
                        "Server hlásí, že nejste jejím členem (například jste opustili tým \
                         nebo vás z něj odebrali). V seznamu se už nezobrazí."
                    )
                    .size(13)
                    .style(muted)
                    .center(),
                ]
                .spacing(8)
                .max_width(420)
                .align_x(iced::Alignment::Center),
            );
            return column![header, notice].into();
        }

        let mut list = Column::new().spacing(3).padding([16, 24]);
        // Some(true) = jsou starší zprávy (na serveru nebo aspoň v cache), None = načítá se
        let older = if self.older_in_store.contains(&conv.id) {
            Some(true)
        } else {
            self.older.get(&conv.id).map(Option::is_some)
        };
        match older {
            Some(true) => {
                list = list.push(
                    container(
                        button(
                            text(if self.loading_older {
                                "Načítám…"
                            } else {
                                "Načíst starší zprávy"
                            })
                            .size(12),
                        )
                        .padding([5, 14])
                        .style(style::secondary)
                        .on_press_maybe((!self.loading_older).then_some(Msg::LoadOlder)),
                    )
                    .center_x(Length::Fill)
                    .padding(iced::Padding::ZERO.bottom(8)),
                );
            }
            None => {
                list = list.push(container(text("Načítám…").style(muted)).center_x(Length::Fill))
            }
            Some(false) => {}
        }
        if self.older.get(&conv.id) == Some(&None)
            && self
                .messages
                .get(&conv.id)
                .is_none_or(|m| m.iter().all(|m| !displayable(m)))
        {
            list = list.push(
                container(
                    column![
                        avatar(&title, 64.0),
                        text(title.clone()).size(16).font(SEMIBOLD),
                        text("Zatím žádné zprávy. Napište první zprávu.").style(muted),
                    ]
                    .spacing(8)
                    .align_x(iced::Alignment::Center),
                )
                .center_x(Length::Fill)
                .padding([40, 0]),
            );
        }
        let msgs = self
            .messages
            .get(&conv.id)
            .map(Vec::as_slice)
            .unwrap_or_default();
        let mut prev: Option<&Message> = None;
        let colors = style::colors(&self.theme());
        let bubble_max = self.bubble_max();
        for m in msgs.iter().filter(|m| displayable(m)) {
            let day = m.arrived_at.map(|t| t.with_timezone(&Local).date_naive());
            let prev_day = prev
                .and_then(|p| p.arrived_at)
                .map(|t| t.with_timezone(&Local).date_naive());
            if let Some(d) = day
                && day != prev_day
            {
                list = list.push(
                    container(
                        container(text(day_label(d)).size(11))
                            .padding([3, 12])
                            .style(style::day_pill),
                    )
                    .center_x(Length::Fill)
                    .padding([12, 0]),
                );
            }
            let new_group = prev.is_none_or(|p| {
                p.sender != m.sender
                    || day != prev_day
                    || match (p.arrived_at, m.arrived_at) {
                        (Some(a), Some(b)) => (b - a).num_minutes() > 5,
                        _ => true,
                    }
            });
            list = list.push(message_view(
                m,
                is_me(&session.profile, m),
                new_group,
                &self.media,
                session.profile.account_id.as_str(),
                MessageUi {
                    hovered: self.hovered_message.as_ref() == Some(&m.id),
                    selecting: self.selecting.as_ref().filter(|s| s.id == m.id),
                    reacting: self.reacting.as_ref() == Some(&m.id),
                    confirm_delete: self.confirm_delete.as_ref() == Some(&m.id),
                    highlighted: self.highlight.as_ref() == Some(&m.id),
                    colors,
                    bubble_max,
                },
            ));
            prev = Some(m);
        }

        let can_send = !self.sending
            && (!self.composer.text().trim().is_empty() || !self.pending_images.is_empty());
        let attachments: Element<'a, Msg> = if self.pending_images.is_empty() {
            space().into()
        } else {
            let thumbs = row(self.pending_images.iter().enumerate().map(|(i, p)| {
                let h = 72.0_f32;
                let w = (p.width as f32 * h / p.height.max(1) as f32).clamp(72.0, 220.0);
                let remove = button(
                    text("✕")
                        .size(11)
                        .center()
                        .width(Length::Fill)
                        .height(Length::Fill),
                )
                .width(22)
                .height(22)
                .padding(0)
                .style(style::remove_chip)
                .on_press(Msg::RemovePending(i));
                stack![
                    container(image(p.preview.clone()).height(h).border_radius(8))
                        .width(w)
                        .height(h)
                        .center_x(w)
                        .style(style::day_pill),
                    container(remove).align_right(w).padding(4),
                ]
                .into()
            }))
            .spacing(8);
            container(
                row![
                    thumbs,
                    text(if self.sending { "Odesílám…" } else { "" })
                        .size(12)
                        .style(muted)
                ]
                .spacing(12)
                .align_y(iced::Alignment::Center),
            )
            .padding(iced::Padding {
                top: 6.0,
                right: 4.0,
                bottom: 2.0,
                left: 4.0,
            })
            .into()
        };
        let banner: Element<'a, Msg> = if self.editing.is_some() {
            composer_banner(
                "Úprava zprávy".into(),
                "Enter uloží, Esc zruší. Formátování zůstane, zmínky se změní na text.".into(),
                Msg::CancelEdit,
                "Zrušit úpravu (Esc)",
            )
        } else if let Some(r) = &self.replying {
            composer_banner(
                format!("Odpověď pro {}", r.author_name),
                truncate_chars(&r.preview, 120),
                Msg::CancelReply,
                "Zrušit odpověď (Esc)",
            )
        } else {
            space().into()
        };
        let mention_candidates = self.mention_candidates();
        let mention_list: Element<'a, Msg> = if mention_candidates.is_empty() {
            space().into()
        } else {
            let items = mention_candidates.iter().enumerate().map(|(i, p)| {
                let selected = i == self.mention_index;
                button(
                    row![avatar(&p.name, 24.0), text(p.name.clone()).size(13)]
                        .spacing(8)
                        .align_y(iced::Alignment::Center),
                )
                .width(Length::Fill)
                .padding([4, 8])
                .style(move |t: &Theme, st| style::list_item(t, st, selected))
                .on_press(Msg::MentionPick(Some(i)))
                .into()
            });
            container(
                column![
                    text("Zmínit (↑↓, Enter, Esc)").size(11).style(muted),
                    Column::with_children(items).spacing(1),
                ]
                .spacing(4),
            )
            .padding([6, 4])
            .max_width(360)
            .into()
        };
        let emoji_panel: Element<'a, Msg> = if self.emoji_open {
            self.emoji_panel()
        } else {
            space().into()
        };
        let composer = container(
            container(column![
                emoji_panel,
                mention_list,
                banner,
                attachments,
                row![
                    text_editor(&self.composer)
                        .id(COMPOSER)
                        .placeholder(
                            "Napište zprávu…  (Shift+Enter nový řádek · **tučně** _kurzíva_ `kód` · @zmínka)",
                        )
                        .on_action(Msg::Compose)
                        .key_binding({
                            let remove_last =
                                self.composer.text().trim_end_matches('\n').is_empty()
                                    && !self.pending_images.is_empty();
                            let keys = ComposerKeys {
                                mention: !mention_candidates.is_empty(),
                                emoji: self.emoji_open,
                                remove_last,
                                cancel: if self.editing.is_some() {
                                    Some(Msg::CancelEdit)
                                } else if self.replying.is_some() {
                                    Some(Msg::CancelReply)
                                } else {
                                    None
                                },
                                edit_last: self.editing.is_none()
                                    && self.composer.text().trim().is_empty(),
                            };
                            move |kp| composer_keys(kp, &keys)
                        })
                        .height(Length::Shrink)
                        .min_height(38)
                        .max_height(160)
                        .padding([9, 6])
                        .style(style::composer_editor),
                    iced::widget::tooltip(
                        button(text("☺").size(18).center().width(Length::Fill))
                            .width(34)
                            .height(38)
                            .padding(0)
                            .style(move |t: &Theme, st| {
                                if self.emoji_open {
                                    style::segment(t, st, true)
                                } else {
                                    style::ghost(t, st)
                                }
                            })
                            .on_press(Msg::ToggleEmoji),
                        container(text("Emoji (Esc zavře)").size(11))
                            .padding([3, 8])
                            .style(style::tooltip),
                        iced::widget::tooltip::Position::Top,
                    ),
                    iced::widget::tooltip(
                        button(
                            text("➤")
                                .size(15)
                                .center()
                                .width(Length::Fill)
                                .height(Length::Fill)
                        )
                        .width(38)
                        .height(38)
                        .padding(0)
                        .style(style::primary_round)
                        .on_press_maybe(can_send.then_some(Msg::Send)),
                        container(text("Odeslat (Enter)").size(11))
                            .padding([3, 8])
                            .style(style::tooltip),
                        iced::widget::tooltip::Position::Top,
                    ),
                ]
                .spacing(8)
                .align_y(iced::Alignment::End),
            ])
            .padding(iced::Padding {
                top: 4.0,
                right: 5.0,
                bottom: 4.0,
                left: 10.0,
            })
            .style(style::composer),
        )
        .padding(iced::Padding {
            top: 8.0,
            right: 20.0,
            bottom: 16.0,
            left: 20.0,
        });

        column![header, hline(), self.thread_view(list), composer]
            .width(Length::Fill)
            .into()
    }
}

impl App {
    /// Vlákno zpráv; když přišla nová zpráva a uživatel je výš, překryje ho tlačítko „Nové zprávy“.
    fn thread_view<'a>(&'a self, list: Column<'a, Msg>) -> Element<'a, Msg> {
        let thread = scrollable(list)
            .id(MESSAGES)
            .on_scroll(Msg::Scrolled)
            .direction(style::scrollbar())
            .style(style::thin_scrollbar)
            .height(Length::Fill);
        let mut layers = stack![thread];
        if self.new_below {
            let pill = button(text("↓  Nové zprávy").size(13).font(SEMIBOLD))
                .padding([8, 18])
                .style(style::primary_round)
                .on_press(Msg::JumpToNewest);
            layers = layers.push(
                container(pill)
                    .align_bottom(Length::Fill)
                    .center_x(Length::Fill)
                    .padding(14),
            );
        }
        if let Some(t) = &self.toast {
            layers = layers.push(
                container(
                    container(text(t.as_str()).size(13))
                        .padding([7, 16])
                        .style(style::toast),
                )
                .center_x(Length::Fill)
                .padding(14),
            );
        }
        layers.into()
    }
}

/// Stav psacího pole pro klávesové zkratky.
struct ComposerKeys {
    /// Je otevřený výběr emoji (Esc ho zavře).
    emoji: bool,
    /// Je otevřená nabídka zmínek (šipky, Enter/Tab, Esc patří jí).
    mention: bool,
    /// Pole je prázdné a jsou vložené obrázky → Backspace odebere poslední.
    remove_last: bool,
    /// Co zruší Esc (úprava, odpověď).
    cancel: Option<Msg>,
    /// Prázdné pole → šipka nahoru upraví poslední vlastní zprávu.
    edit_last: bool,
}

/// Lišta nad psacím polem (odpověď, úprava) s tlačítkem zrušení.
fn composer_banner<'a>(
    title: String,
    detail: String,
    cancel: Msg,
    tip: &'a str,
) -> Element<'a, Msg> {
    container(
        container(
            container(
                row![
                    column![
                        text(title).size(12).font(SEMIBOLD),
                        text(detail).size(12).style(muted).wrapping(Wrapping::None),
                    ]
                    .spacing(1)
                    .width(Length::Fill),
                    icon_button("✕", Some(cancel), tip),
                ]
                .spacing(10)
                .align_y(iced::Alignment::Center),
            )
            .padding(iced::Padding {
                top: 4.0,
                right: 4.0,
                bottom: 4.0,
                left: 10.0,
            })
            .clip(true)
            .style(style::reply_preview),
        )
        .padding(iced::Padding {
            left: 3.0,
            ..iced::Padding::ZERO
        })
        .style(style::reply_frame),
    )
    .padding(iced::Padding {
        top: 6.0,
        right: 4.0,
        bottom: 2.0,
        left: 0.0,
    })
    .into()
}

/// Enter odešle, Shift+Enter vloží nový řádek; ostatní klávesy beze změny.
/// `remove_last`: pole je prázdné a jsou vložené obrázky → Backspace odebere poslední.
fn composer_keys(
    kp: text_editor::KeyPress,
    keys: &ComposerKeys,
) -> Option<text_editor::Binding<Msg>> {
    use iced::keyboard::{Key, key::Named};
    if keys.mention && kp.modifiers.is_empty() {
        let msg = match kp.key {
            Key::Named(Named::ArrowUp) => Some(Msg::MentionMove(-1)),
            Key::Named(Named::ArrowDown) => Some(Msg::MentionMove(1)),
            Key::Named(Named::Enter | Named::Tab) => Some(Msg::MentionPick(None)),
            Key::Named(Named::Escape) => Some(Msg::MentionClose),
            _ => None,
        };
        if let Some(msg) = msg {
            return Some(text_editor::Binding::Custom(msg));
        }
    }
    if keys.emoji && kp.key == Key::Named(Named::Escape) {
        return Some(text_editor::Binding::Custom(Msg::ToggleEmoji));
    }
    if let Some(cancel) = &keys.cancel
        && kp.key == Key::Named(Named::Escape)
    {
        return Some(text_editor::Binding::Custom(cancel.clone()));
    }
    if keys.edit_last && kp.key == Key::Named(Named::ArrowUp) && kp.modifiers.is_empty() {
        return Some(text_editor::Binding::Custom(Msg::EditLast));
    }
    if keys.remove_last && kp.key == Key::Named(Named::Backspace) {
        return Some(text_editor::Binding::Custom(Msg::RemoveLastPending));
    }
    if kp.key == Key::Named(Named::Enter) && !kp.modifiers.shift() {
        return Some(text_editor::Binding::Custom(Msg::Send));
    }
    // Ctrl+V: schránku čteme sami, aby šlo vložit i obrázek
    let paste = matches!(&kp.key, Key::Character(c) if c.as_str().eq_ignore_ascii_case("v"))
        && kp.modifiers.command()
        || kp.key == Key::Named(Named::Insert) && kp.modifiers.shift();
    if paste {
        return Some(text_editor::Binding::Custom(Msg::Paste));
    }
    text_editor::Binding::from_key_press(kp)
}

fn tab_button<'a>(label: &str, unread: usize, active: bool, tab: Tab) -> Element<'a, Msg> {
    let font = if active { SEMIBOLD } else { Font::DEFAULT };
    let mut content = row![text(label.to_owned()).size(13).font(font)]
        .spacing(6)
        .align_y(iced::Alignment::Center);
    if unread > 0 {
        content = content.push(badge(unread));
    }
    button(container(content).center_x(Length::Fill))
        .width(Length::Fill)
        .padding([6, 10])
        .style(move |t: &Theme, status| style::segment(t, status, active))
        .on_press(Msg::SetTab(tab))
        .into()
}

/// Odznak s počtem nepřečtených („99+“ nad 99).
fn badge<'a>(count: usize) -> Element<'a, Msg> {
    let label = if count > 99 {
        "99+".to_owned()
    } else {
        count.to_string()
    };
    container(text(label).size(10).font(BOLD))
        .padding([1, 6])
        .style(style::badge)
        .into()
}

/// Tečka nepřečtené konverzace (prázdné místo, když je přečtená, aby řádky neposkakovaly).
fn unread_dot<'a>(unread: bool) -> Element<'a, Msg> {
    let dot = container(space())
        .width(9)
        .height(9)
        .style(move |t: &Theme| container::Style {
            background: unread.then(|| style::colors(t).primary.into()),
            border: Border {
                radius: 5.0.into(),
                ..Default::default()
            },
            ..Default::default()
        });
    dot.into()
}

fn section_label<'a>(label: &'a str) -> Element<'a, Msg> {
    container(text(label).size(10).font(BOLD).style(muted))
        .padding(iced::Padding {
            top: 12.0,
            right: 10.0,
            bottom: 4.0,
            left: 10.0,
        })
        .into()
}

/// Schůzka z formuláře, nebo popis chyby pro uživatele.
fn meeting_from_form(f: &MeetingForm) -> Result<tymy_core::NewMeeting, String> {
    let today = Local::now().date_naive();
    if f.subject.trim().is_empty() {
        return Err("Zadejte název schůzky.".into());
    }
    let day = cal::parse_date(&f.date, today).ok_or("Datum zadejte jako 10. 11. 2026.")?;
    let start = cal::parse_time(&f.start).ok_or("Začátek zadejte jako 9:30.")?;
    let end = cal::parse_time(&f.end).ok_or("Konec zadejte jako 10:00.")?;
    let start = cal::local_to_utc(day, start).ok_or("Neplatný čas začátku.")?;
    let end = cal::local_to_utc(day, end).ok_or("Neplatný čas konce.")?;
    if end <= start {
        return Err("Konec musí být po začátku.".into());
    }
    let mut attendees = vec![];
    for p in &f.attendees {
        let email = p
            .email
            .clone()
            .filter(|e| e.contains('@'))
            .ok_or_else(|| format!("{} nemá e-mail, pozvánku nelze poslat.", p.display_name))?;
        attendees.push(tymy_core::MeetingAttendee {
            email,
            name: p.display_name.clone(),
        });
    }
    Ok(tymy_core::NewMeeting {
        subject: f.subject.trim().to_owned(),
        start,
        end,
        attendees,
        online: f.online,
        body: Some(f.body.clone()),
        location: Some(f.location.clone()),
        reminder_minutes: 15,
    })
}

/// Formulář nové schůzky (místo detailu události).
fn meeting_form_view(f: &MeetingForm) -> Element<'_, Msg> {
    let header = container(
        column![
            text("Nová schůzka").size(16).font(BOLD),
            text(if f.attendees.is_empty() {
                "Bez účastníků se uloží jen do vašeho kalendáře."
            } else {
                "Účastníci dostanou pozvánku e-mailem a v Teams."
            })
            .size(12)
            .style(muted),
        ]
        .spacing(1),
    )
    .padding([12, 20])
    .width(Length::Fill)
    .style(style::header);
    let field =
        |label: &'static str, placeholder: &'static str, value: &str, kind: MeetingField| {
            column![
                text(label).size(12).style(muted),
                text_input(placeholder, value)
                    .on_input(move |v| Msg::MeetingInput(kind, v))
                    .on_submit(Msg::MeetingCreate)
                    .padding([8, 12])
                    .size(13)
                    .style(style::search_input),
            ]
            .spacing(4)
        };
    let chips = iced::widget::Row::with_children(f.attendees.iter().map(|p| {
        button(
            row![
                avatar(&p.display_name, 20.0),
                text(truncate(&p.display_name, 24)).size(12),
                text("✕").size(11).style(muted),
            ]
            .spacing(6)
            .align_y(iced::Alignment::Center),
        )
        .padding([3, 8])
        .style(style::secondary)
        .on_press(Msg::MeetingRemove(p.mri.clone()))
        .into()
    }))
    .spacing(6)
    .wrap();
    let mut results = Column::new().spacing(2);
    if f.searching {
        results = results.push(note("Hledám…"));
    } else if f.query.trim().chars().count() >= MIN_PEOPLE_QUERY && f.results.is_empty() {
        results = results.push(note("Nikdo nenalezen"));
    }
    for p in f
        .results
        .iter()
        .filter(|p| !f.attendees.iter().any(|a| a.mri == p.mri))
    {
        results = results.push(
            button(
                row![
                    avatar(&p.display_name, 28.0),
                    column![
                        text(truncate(&p.display_name, 32)).size(13).font(SEMIBOLD),
                        text(truncate(&p.subtitle(), 48)).size(11).style(muted),
                    ]
                    .spacing(1),
                ]
                .spacing(10)
                .align_y(iced::Alignment::Center),
            )
            .width(Length::Fill)
            .padding([6, 8])
            .style(|t: &Theme, st| style::list_item(t, st, false))
            .on_press(Msg::MeetingAdd(p.clone())),
        );
    }
    let online = button(
        row![
            text(if f.online { "☑" } else { "☐" }).size(16),
            text("Teams schůzka (odkaz Připojit)").size(13),
        ]
        .spacing(8)
        .align_y(iced::Alignment::Center),
    )
    .padding([4, 6])
    .style(style::ghost)
    .on_press(Msg::MeetingOnline(!f.online));
    let send_label = if f.busy {
        "Ukládám…"
    } else if f.attendees.is_empty() {
        "Uložit"
    } else {
        "Odeslat pozvánky"
    };
    let body = column![
        field(
            "Název",
            "Např. Porada týmu",
            &f.subject,
            MeetingField::Subject
        ),
        row![
            field("Datum", "10. 11. 2026", &f.date, MeetingField::Date)
                .width(Length::FillPortion(2)),
            field("Od", "9:30", &f.start, MeetingField::Start).width(Length::FillPortion(1)),
            field("Do", "10:00", &f.end, MeetingField::End).width(Length::FillPortion(1)),
        ]
        .spacing(10),
        text(format!("Účastníci ({})", f.attendees.len()))
            .size(12)
            .style(muted),
        chips,
        text_input("Pozvat: jméno nebo e-mail…", &f.query)
            .on_input(|v| Msg::MeetingInput(MeetingField::Query, v))
            .padding([8, 12])
            .size(13)
            .style(style::search_input),
        results,
        online,
        field(
            "Místo (nepovinné)",
            "Např. zasedačka",
            &f.location,
            MeetingField::Location
        ),
        field(
            "Popis (nepovinný)",
            "Program schůzky…",
            &f.body,
            MeetingField::Body
        ),
        text(f.error.clone().unwrap_or_default())
            .size(13)
            .style(style::danger),
        row![
            button(text("Zrušit").size(13))
                .padding([7, 14])
                .style(style::ghost)
                .on_press(Msg::MeetingClose),
            button(text(send_label).size(13).font(SEMIBOLD))
                .padding([7, 16])
                .style(style::primary)
                .on_press_maybe((!f.busy).then_some(Msg::MeetingCreate)),
        ]
        .spacing(10),
    ]
    .spacing(10)
    .padding([16, 24])
    .max_width(640);
    column![
        header,
        scrollable(body)
            .direction(style::scrollbar())
            .style(style::thin_scrollbar)
            .height(Length::Fill),
    ]
    .into()
}

/// Nadpis sekce s vlastním textem (např. den v agendě).
fn section_title<'a>(label: String) -> Element<'a, Msg> {
    container(text(label).size(10).font(BOLD).style(muted))
        .padding(iced::Padding {
            top: 12.0,
            right: 10.0,
            bottom: 4.0,
            left: 10.0,
        })
        .into()
}

/// Položka agendy: barevný proužek, název a čas · místo.
fn event_item<'a>(e: &CalendarEvent, now: DateTime<Utc>, selected: bool) -> Element<'a, Msg> {
    let ongoing = e.start <= now && now < e.end;
    let dim = e.end <= now || e.cancelled || e.response == EventResponse::Declined;
    let subject = if e.cancelled {
        format!("Zrušeno: {}", e.subject)
    } else {
        e.subject.clone()
    };
    let mut detail = cal::time_label(e);
    if let Some(place) = e.location.as_ref().or(e.organizer.as_ref()) {
        detail.push_str(" · ");
        detail.push_str(place);
    }
    let bar = container(space())
        .width(3)
        .height(34)
        .style(move |t: &Theme| {
            let c = style::colors(t);
            container::Style {
                background: Some(
                    if dim {
                        c.border
                    } else if ongoing {
                        c.primary
                    } else {
                        style::mix(c.primary, c.surface, 0.45)
                    }
                    .into(),
                ),
                border: Border {
                    radius: 2.0.into(),
                    ..Default::default()
                },
                ..Default::default()
            }
        });
    button(
        row![
            bar,
            column![
                text(truncate(&subject, 30))
                    .wrapping(Wrapping::None)
                    .size(13)
                    .font(SEMIBOLD)
                    .style(move |t: &Theme| if dim {
                        muted(t)
                    } else {
                        text::Style::default()
                    }),
                text(truncate(&detail, 36))
                    .wrapping(Wrapping::None)
                    .size(11)
                    .style(if ongoing && !dim { accent } else { muted }),
            ]
            .spacing(3),
        ]
        .spacing(10)
        .align_y(iced::Alignment::Center),
    )
    .width(Length::Fill)
    .padding([7, 8])
    .style(move |t: &Theme, status| style::list_item(t, status, selected))
    .on_press(Msg::SelectEvent(e.id.clone()))
    .into()
}

/// Blok události v týdenní mřížce (`all_day` = řádek celodenních událostí).
fn event_block<'a>(
    e: &CalendarEvent,
    width: f32,
    height: f32,
    selected: bool,
    now: DateTime<Utc>,
    all_day: bool,
) -> Element<'a, Msg> {
    let kind = if e.cancelled || e.response == EventResponse::Declined {
        style::EventKind::Dim
    } else if matches!(
        e.response,
        EventResponse::Tentative | EventResponse::NotResponded
    ) {
        style::EventKind::Tentative
    } else {
        style::EventKind::Normal
    };
    let past = e.end <= now;
    let subject = if e.cancelled {
        format!("Zrušeno: {}", e.subject)
    } else {
        e.subject.clone()
    };
    let title = text(subject).size(11).font(SEMIBOLD);
    let content: Element<'a, Msg> = if all_day || height < 34.0 {
        title.wrapping(Wrapping::None).into()
    } else {
        let start = e.start.with_timezone(&Local).format("%H:%M");
        let end = e.end.with_timezone(&Local).format("%H:%M");
        column![title, text(format!("{start}–{end}")).size(10).style(muted)]
            .spacing(1)
            .into()
    };
    button(
        container(content)
            .clip(true)
            .width(Length::Fill)
            .height(Length::Fill),
    )
    .width(width)
    .height(height)
    .padding([2, 5])
    .style(move |t: &Theme, st| style::event_block(t, st, kind, selected, past))
    .on_press(Msg::SelectEvent(e.id.clone()))
    .into()
}

/// „Kalendářní list“ s dnem v týdnu a číslem dne (hlavička detailu události).
fn date_badge<'a>(day: chrono::NaiveDate) -> Element<'a, Msg> {
    use chrono::Datelike;
    const DAYS: [&str; 7] = ["PO", "ÚT", "ST", "ČT", "PÁ", "SO", "NE"];
    container(
        column![
            text(DAYS[day.weekday().num_days_from_monday() as usize])
                .size(9)
                .font(BOLD)
                .style(accent),
            text(day.day().to_string()).size(16).font(BOLD),
        ]
        .align_x(iced::Alignment::Center),
    )
    .width(38)
    .height(38)
    .center_x(38)
    .center_y(38)
    .style(|t: &Theme| container::Style {
        background: Some(style::colors(t).primary_soft.into()),
        border: Border {
            radius: 9.0.into(),
            ..Default::default()
        },
        ..Default::default()
    })
    .into()
}

fn note<'a>(t: &str) -> Element<'a, Msg> {
    container(text(t.to_owned()).size(12).style(muted))
        .padding([6, 10])
        .into()
}

/// Malé průhledné tlačítko s ikonou a popiskem při najetí myší.
/// Šířka ikonového tlačítka (kvůli rezervě místa vedle bubliny).
const ICON_BUTTON: f32 = 26.0;

/// Reakce nabízené ve výběru (klíče Teams).
const REACTIONS: [(&str, &str); 6] = [
    ("like", "👍"),
    ("heart", "❤️"),
    ("laugh", "😆"),
    ("surprised", "😮"),
    ("sad", "😢"),
    ("angry", "😠"),
];

fn icon_button<'a>(label: &'a str, msg: Option<Msg>, tip: &'a str) -> Element<'a, Msg> {
    iced::widget::tooltip(
        button(text(label).size(12).center().width(Length::Fill))
            .width(ICON_BUTTON)
            .padding([3, 0])
            .style(style::ghost)
            .on_press_maybe(msg),
        container(text(tip).size(11))
            .padding([3, 8])
            .style(style::tooltip),
        iced::widget::tooltip::Position::Bottom,
    )
    .into()
}

/// Tlačítko „Kopírovat“ (do schránky i primárního výběru) s potvrzením `toast`.
fn copy_button<'a>(value: &str, toast: &'static str) -> Element<'a, Msg> {
    button(text("Kopírovat").size(12))
        .padding([4, 10])
        .style(style::secondary)
        .on_press(Msg::CopyText(value.to_owned(), toast))
        .into()
}

/// Co ukázat u účtu v menu vpravo.
enum AccountStatus {
    Unread(usize),
    Note(String),
}

/// Položka účtu v menu: avatar, jméno, e-mail a typ, nepřečtené; aktivní účet má ✓.
fn account_item<'a>(
    a: &AccountRecord,
    active: bool,
    status: Option<AccountStatus>,
    msg: Msg,
) -> Element<'a, Msg> {
    let name = a.display_name.clone().unwrap_or_else(|| "Účet".into());
    let kind = match a.kind {
        AccountKind::Work => "pracovní",
        AccountKind::Personal => "osobní",
    };
    let detail = match &a.username {
        Some(u) => format!("{} · {kind}", truncate(u, 26)),
        None => kind.to_owned(),
    };
    button(
        row![
            avatar(&name, 28.0),
            column![
                text(truncate(&name, 24)).size(13).font(SEMIBOLD),
                text(detail).size(11).style(muted),
            ]
            .spacing(1)
            .width(Length::Fill),
            match status {
                Some(AccountStatus::Unread(n)) if n > 0 => badge(n),
                Some(AccountStatus::Note(t)) => text(t).size(11).style(muted).into(),
                _ => space().width(0).into(),
            },
            text(if active { "✓" } else { "" })
                .size(13)
                .style(style::accent),
        ]
        .spacing(10)
        .align_y(iced::Alignment::Center),
    )
    .padding([6, 8])
    .width(Length::Fill)
    .style(move |t: &Theme, st| style::list_item(t, st, active))
    .on_press(msg)
    .into()
}

/// Kulatý avatar s iniciálami a barvou odvozenou ze jména.
fn avatar<'a>(name: &str, size: f32) -> Element<'a, Msg> {
    container(text(style::initials(name)).size(size * 0.38).font(SEMIBOLD))
        .width(size)
        .height(size)
        .center_x(size)
        .center_y(size)
        .style(style::avatar(style::avatar_color(name), size / 2.0))
        .into()
}

/// Avatar týmu: zaoblený čtverec.
fn team_avatar<'a>(name: &str, size: f32) -> Element<'a, Msg> {
    container(text(style::initials(name)).size(size * 0.4).font(BOLD))
        .width(size)
        .height(size)
        .center_x(size)
        .center_y(size)
        .style(style::avatar(style::avatar_color(name), size * 0.28))
        .into()
}

/// Logo aplikace (bublina) pro prázdné stavy a přihlášení.
fn logo<'a>(size: f32) -> Element<'a, Msg> {
    container(text("💬").size(size * 0.55))
        .width(size)
        .height(size)
        .center_x(size)
        .center_y(size)
        .style(move |t: &Theme| container::Style {
            background: Some(style::colors(t).primary_soft.into()),
            border: Border {
                radius: (size * 0.3).into(),
                ..Default::default()
            },
            ..Default::default()
        })
        .into()
}

/// Svislá a vodorovná dělicí linka v barvě okrajů.
fn vline<'a>() -> Element<'a, Msg> {
    container(space())
        .width(1)
        .height(Length::Fill)
        .style(style::divider)
        .into()
}

fn hline<'a>() -> Element<'a, Msg> {
    container(space())
        .width(Length::Fill)
        .height(1)
        .style(style::divider)
        .into()
}

fn looks_like_email(q: &str) -> bool {
    let q = q.trim();
    q.split_once('@')
        .is_some_and(|(user, domain)| !user.is_empty() && domain.contains('.') && !q.contains(' '))
}

fn person_item(p: &Person) -> Element<'_, Msg> {
    let mut title = row![text(truncate(&p.display_name, 24)).size(14).font(SEMIBOLD)]
        .spacing(6)
        .align_y(iced::Alignment::Center);
    if p.external {
        title = title.push(text("externí").size(10).font(BOLD).style(accent));
    }
    let subtitle = p.subtitle();
    button(
        row![
            avatar(&p.display_name, 34.0),
            column![title, text(truncate(&subtitle, 34)).size(12).style(muted)].spacing(2),
        ]
        .spacing(10)
        .align_y(iced::Alignment::Center),
    )
    .width(Length::Fill)
    .padding([7, 8])
    .style(|t: &Theme, status| style::list_item(t, status, false))
    .on_press(Msg::OpenPerson(p.clone()))
    .into()
}

/// Id zprávy jako čas v ms (0 = neznámé).
fn id_ms(m: &Message) -> i64 {
    m.id.as_str().parse().unwrap_or(0)
}

fn displayable(m: &Message) -> bool {
    m.deleted
        || m.is_user_content()
        || matches!(&m.kind, MessageKind::Event(t) if t.starts_with("Event/Call"))
}

/// Rozpracovaná úprava vlastní zprávy.
#[derive(Debug, Clone)]
struct Editing {
    conversation: ConversationId,
    id: MessageId,
    /// Původní HTML (zachová se citace odpovědi a obrázky).
    original: String,
    /// Text, který byl v psacím poli před úpravou (vrátí se po ní).
    draft: String,
}

/// Stav zprávy v UI (najetí myší, režim výběru).
struct MessageUi<'a> {
    hovered: bool,
    selecting: Option<&'a Selecting>,
    /// Otevřený výběr reakcí.
    reacting: bool,
    /// Čeká se na potvrzení smazání.
    confirm_delete: bool,
    /// Zpráva nalezená hledáním.
    highlighted: bool,
    /// Barvy aktuálního tématu (úseky formátovaného textu potřebují konkrétní barvy).
    colors: style::Colors,
    /// Nejširší bublina (podle šířky okna).
    bubble_max: f32,
}

/// Prostý text zprávy pro kopírování (bez obrázků; přílohy jako názvy).
fn message_plain(m: &Message) -> String {
    let mut parts: Vec<String> = vec![rich::plain(&rich::parse(&m.content))];
    parts.retain(|p| !p.is_empty());
    parts.extend(m.attachments.iter().map(|a| a.name.clone()));
    parts.join("\n\n")
}

/// Odhad šířky textu (px) pro editor v režimu výběru; raději o něco širší.
fn estimate_text_width(text: &str, max: f32) -> f32 {
    let longest = text.lines().map(|l| l.chars().count()).max().unwrap_or(0) as f32;
    (longest * 8.2 + 8.0).clamp(80.0, max)
}

/// Nejširší bublina: širší řádky textu se už špatně čtou.
const BUBBLE_MAX: f32 = 1100.0;
/// Nejužší bublina (i v úzkém okně).
const BUBBLE_MIN: f32 = 320.0;
/// Vodorovné odsazení textu v bublině (13 px z každé strany).
const BUBBLE_PADDING: f32 = 26.0;

/// Šířka místa pro avatar u zpráv ostatních.
const MESSAGE_AVATAR: f32 = 30.0;

fn message_view<'a>(
    m: &'a Message,
    mine: bool,
    header: bool,
    media: &'a HashMap<String, Media>,
    me: &str,
    ui: MessageUi<'a>,
) -> Element<'a, Msg> {
    // tlumený text uvnitř bubliny: na barevné (vlastní) bublině průhledná bílá
    let soft = move |t: &Theme| text::Style {
        color: Some(if mine {
            Color {
                a: 0.75,
                ..style::colors(t).on_mine
            }
        } else {
            style::colors(t).muted
        }),
    };
    let mut body = Column::new().spacing(6);
    if m.deleted {
        body = body.push(text("Zpráva byla smazána").size(13).style(soft));
    } else if let MessageKind::Event(_) = m.kind {
        body = body.push(text("📞 Hovor").size(13).style(soft));
    } else if let Some(sel) = ui.selecting {
        // režim výběru: text v editoru jen pro čtení (obrázky se mezitím nezobrazují)
        body = body.push(
            text_editor(&sel.content)
                .on_action(Msg::SelectAction)
                .width(sel.width)
                .padding(0)
                .size(14)
                .line_height(1.35)
                .style(move |t: &Theme, _| style::selectable_text(t, mine)),
        );
    } else {
        body = body.extend(blocks_view(
            &rich::parse(&m.content),
            mine,
            false,
            media,
            &ui.colors,
            ui.bubble_max - BUBBLE_PADDING,
        ));
        for a in &m.attachments {
            let label = text(format!("📎 {}", a.name)).size(13).font(SEMIBOLD);
            body = body.push(match &a.url {
                Some(url) => Element::from(
                    button(label)
                        .style(move |t: &Theme, s| style::link(t, s, mine))
                        .padding(0)
                        .on_press(Msg::OpenUrl(url.clone())),
                ),
                None => label.into(),
            });
        }
        if m.edited_at.is_some() {
            body = body.push(text("upraveno").size(10).style(soft));
        }
    }
    let selecting = ui.selecting.is_some();
    let bubble: Element<'a, Msg> = mouse_area(
        container(body)
            .padding([8, 13])
            .max_width(ui.bubble_max)
            .style(move |t: &Theme| style::bubble(t, mine, header)),
    )
    .on_double_click(Msg::StartSelect(m.id.clone()))
    .into();
    // tlačítka vedle bubliny: při najetí myší, v režimu výběru stále
    let can_copy = !m.deleted && m.is_user_content();
    let editable = mine && can_copy;
    let reserved = if editable { 6.0 } else { 4.0 } * ICON_BUTTON;
    let actions: Element<'a, Msg> = if ui.confirm_delete {
        row![
            button(text("Smazat").size(12).font(SEMIBOLD))
                .padding([3, 10])
                .style(style::danger_button)
                .on_press(Msg::Delete(m.id.clone())),
            button(text("Zrušit").size(12))
                .padding([3, 10])
                .style(style::ghost)
                .on_press(Msg::CancelDelete),
        ]
        .spacing(4)
        .align_y(iced::Alignment::Center)
        .into()
    } else if selecting {
        row![
            icon_button(
                "⧉",
                Some(Msg::CopyMessage(m.id.clone())),
                "Kopírovat celou zprávu"
            ),
            icon_button("✓", Some(Msg::StopSelect), "Hotovo"),
        ]
        .into()
    } else if (ui.hovered || ui.reacting) && can_copy {
        let mut buttons = vec![
            icon_button("☺", Some(Msg::ToggleReactions(m.id.clone())), "Reagovat"),
            icon_button("↩", Some(Msg::Reply(m.id.clone())), "Odpovědět"),
            icon_button(
                "⧉",
                Some(Msg::CopyMessage(m.id.clone())),
                "Kopírovat zprávu",
            ),
            icon_button(
                "⌶",
                Some(Msg::StartSelect(m.id.clone())),
                "Označit text (nebo dvojklik na zprávu)",
            ),
        ];
        if editable {
            buttons.insert(
                0,
                icon_button(
                    "✎",
                    Some(Msg::StartEdit(m.id.clone())),
                    "Upravit (↑ v prázdném poli)",
                ),
            );
            buttons.insert(
                1,
                icon_button("🗑", Some(Msg::AskDelete(m.id.clone())), "Smazat"),
            );
        }
        iced::widget::Row::with_children(buttons).into()
    } else {
        space().width(reserved).into()
    };
    let bubble: Element<'a, Msg> = if mine {
        row![actions, bubble]
            .spacing(4)
            .align_y(iced::Alignment::Center)
            .into()
    } else {
        row![bubble, actions]
            .spacing(4)
            .align_y(iced::Alignment::Center)
            .into()
    };

    let mut col = Column::new().spacing(3);
    if header {
        let time = short_time(m.arrived_at.or(m.composed_at));
        let label: Element<'a, Msg> = if mine {
            text(time).size(11).style(muted).into()
        } else {
            row![
                text(m.sender_name.as_deref().unwrap_or("?"))
                    .size(12)
                    .font(SEMIBOLD),
                text(time).size(11).style(muted),
            ]
            .spacing(8)
            .align_y(iced::Alignment::Center)
            .into()
        };
        col = col.push(if mine {
            Element::from(row![space::horizontal(), label])
        } else {
            label
        });
    }
    col = col.push(if mine {
        Element::from(row![space::horizontal(), bubble])
    } else {
        Element::from(row![bubble, space::horizontal()])
    });
    if ui.reacting && can_copy {
        let picker = row(REACTIONS.iter().map(|(key, emoji)| {
            let by_me = m
                .reactions
                .iter()
                .any(|r| r.key == *key && r.users.iter().any(|u| u.as_str().ends_with(me)));
            button(text(*emoji).size(18).line_height(1.1))
                .padding([2, 6])
                .style(move |t: &Theme, st| style::reaction_button(t, st, by_me))
                .on_press(Msg::React(m.id.clone(), (*key).to_owned()))
                .into()
        }))
        .spacing(2);
        let picker = container(picker).padding(3).style(style::reaction_picker);
        col = col.push(if mine {
            Element::from(row![space::horizontal(), picker])
        } else {
            Element::from(row![space().width(4), picker])
        });
    }
    if !m.deleted && !m.reactions.is_empty() {
        let chips = row(m.reactions.iter().map(|r| {
            let by_me = r.users.iter().any(|u| u.as_str().ends_with(me));
            let label = if r.users.len() > 1 {
                format!("{} {}", r.emoji(), r.users.len())
            } else {
                r.emoji()
            };
            let chip = button(text(label).size(15).line_height(1.1))
                .padding([1, 8])
                .style(move |t: &Theme, st| style::reaction_button(t, st, by_me));
            if can_copy {
                chip.on_press(Msg::React(m.id.clone(), r.key.clone()))
                    .into()
            } else {
                chip.into()
            }
        }))
        .spacing(4);
        col = col.push(if mine {
            Element::from(row![space::horizontal(), chips])
        } else {
            Element::from(row![space().width(4), chips])
        });
    }

    // u ostatních: avatar u první zprávy skupiny, jinak odsazení
    let line: Element<'a, Msg> = if mine {
        row![space().width(MESSAGE_AVATAR + 40.0), col].into()
    } else {
        let side: Element<'a, Msg> = if header {
            avatar(m.sender_name.as_deref().unwrap_or("?"), MESSAGE_AVATAR)
        } else {
            space().width(MESSAGE_AVATAR).into()
        };
        row![side, col, space().width(40)].spacing(10).into()
    };
    let top = if header { 10.0 } else { 0.0 };
    let highlighted = ui.highlighted;
    mouse_area(
        container(
            container(line)
                .padding(if highlighted { 4 } else { 0 })
                .style(move |t: &Theme| {
                    if highlighted {
                        style::highlight(t)
                    } else {
                        container::Style::default()
                    }
                }),
        )
        .padding(iced::Padding {
            top,
            right: 0.0,
            bottom: 0.0,
            left: 0.0,
        }),
    )
    .on_enter(Msg::HoverMessage(m.id.clone()))
    .on_exit(Msg::UnhoverMessage(m.id.clone()))
    .into()
}

/// Vykreslí bloky obsahu zprávy (odstavce, seznamy, citace, kód, tabulky, obrázky).
/// `quoted`: obsah citace (menší písmo).
fn blocks_view<'a>(
    blocks: &[Block],
    mine: bool,
    quoted: bool,
    media: &'a HashMap<String, Media>,
    c: &style::Colors,
    width: f32,
) -> Vec<Element<'a, Msg>> {
    blocks
        .iter()
        .map(|b| block_view(b, mine, quoted, media, c, width))
        .collect()
}

fn block_view<'a>(
    block: &Block,
    mine: bool,
    quoted: bool,
    media: &'a HashMap<String, Media>,
    c: &style::Colors,
    // nejvyšší šířka obsahu (px)
    width: f32,
) -> Element<'a, Msg> {
    let size = if quoted { 13.0 } else { 14.0 };
    match block {
        Block::Paragraph(spans) => paragraph(spans, size, false, mine, c),
        Block::Heading(spans) => paragraph(spans, size + 2.0, true, mine, c),
        Block::ListItem {
            depth,
            marker,
            spans,
        } => row![
            space().width(*depth as f32 * 14.0 - 14.0),
            text(marker.clone())
                .size(size)
                .line_height(1.35)
                .width(if marker.ends_with('.') { 22 } else { 14 }),
            paragraph(spans, size, false, mine, c),
        ]
        .into(),
        Block::Code {
            language,
            text: code,
        } => {
            let mut col = Column::new().spacing(4);
            if let Some(lang) = language {
                col = col.push(
                    text(lang.clone())
                        .size(10)
                        .color(style::bubble_tint(c, mine, 0.6)),
                );
            }
            col = col.push(
                text(code.clone())
                    .font(Font::MONOSPACE)
                    .size(13)
                    .line_height(1.3),
            );
            container(col)
                .padding([8, 10])
                .style(move |t: &Theme| style::code_block(t, mine))
                .into()
        }
        Block::Quote(q) => {
            let mut col = Column::new().spacing(3);
            let label_color = style::bubble_tint(c, mine, 0.7);
            match &q.kind {
                QuoteKind::Reply { author, .. } => {
                    col = col.push(
                        text(author.clone().unwrap_or_else(|| "Odpověď".into()))
                            .size(12)
                            .font(SEMIBOLD),
                    );
                }
                QuoteKind::Forward => {
                    col = col.push(text("↪ Přeposláno").size(11).color(label_color));
                }
                QuoteKind::Plain => {}
            }
            let inner = blocks_view(&q.blocks, mine, true, media, c, width - 23.0);
            col = col.extend(inner);
            let quote = container(
                container(col)
                    .padding([5, 10])
                    .style(move |t: &Theme| style::quote(t, mine)),
            )
            .padding(iced::Padding {
                left: 3.0,
                ..iced::Padding::ZERO
            })
            .style(move |t: &Theme| style::quote_frame(t, mine));
            match &q.kind {
                // klik na citaci odpovědi skočí na původní zprávu
                QuoteKind::Reply {
                    message_id: Some(id),
                    ..
                } => mouse_area(quote)
                    .on_press(Msg::JumpToQuoted(MessageId::from(id.as_str())))
                    .interaction(iced::mouse::Interaction::Pointer)
                    .into(),
                _ => quote.into(),
            }
        }
        Block::Table(rows) => {
            // Pevné šířky sloupců podle obsahu (s `Fill` by tabulka zabrala celou bublinu
            // a sloupce by se dělily rovným dílem bez ohledu na obsah).
            let cols = table_columns(rows, width);
            let table = Column::with_children(rows.iter().map(|r| {
                iced::widget::Row::with_children(r.iter().zip(&cols).map(|(cell, w)| {
                    let header = cell.header;
                    container(paragraph(&cell.spans, 13.0, header, mine, c))
                        .padding([4, 8])
                        .width(*w)
                        .style(move |t: &Theme| style::table_cell(t, mine, header))
                        .into()
                }))
                .into()
            }));
            table.into()
        }
        Block::Image(img) => image_view(img, media.get(&img.url)),
        Block::Rule => container(space())
            .width(width.min(BUBBLE_MIN))
            .height(1)
            .style(move |t: &Theme| style::rule(t, mine))
            .into(),
        Block::Blank => space().height(2).into(),
    }
}

/// Šířky sloupců tabulky (px): podle nejdelšího textu ve sloupci, dohromady nejvýš `max`.
/// Když se nevejdou, zúží se jen široké sloupce (úzké, např. čísla, zůstanou celé).
fn table_columns(rows: &[Vec<rich::Cell>], max: f32) -> Vec<f32> {
    // odhad šířky znaku písma 13 px (tučné záhlaví o něco širší) + odsazení buňky 2 × 8 px
    const CHAR: f32 = 7.4;
    const PAD: f32 = 18.0;
    let n = rows.iter().map(Vec::len).max().unwrap_or(0);
    let mut cols = vec![40.0_f32; n];
    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            let chars: usize = cell.spans.iter().map(|s| s.text.chars().count()).sum();
            let factor = if cell.header { 1.08 } else { 1.0 };
            cols[i] = cols[i].max(chars as f32 * CHAR * factor + PAD);
        }
    }
    let total: f32 = cols.iter().sum();
    if total > max && n > 0 {
        // Rovný podíl na sloupec; sloupce užší než podíl si nechají svou šířku a zbytek
        // se rozdělí mezi široké podle jejich poměru.
        let fair = max / n as f32;
        let narrow: f32 = cols.iter().filter(|w| **w <= fair).sum();
        let wide: f32 = cols.iter().filter(|w| **w > fair).sum();
        let room = (max - narrow).max(0.0);
        for w in &mut cols {
            if *w > fair {
                *w = (*w / wide * room).max(40.0);
            }
        }
    }
    cols
}

/// Odstavec formátovaného textu; klik na odkaz ho otevře v prohlížeči.
fn paragraph<'a>(
    spans: &[rich::Span],
    size: f32,
    bold: bool,
    mine: bool,
    c: &style::Colors,
) -> Element<'a, Msg> {
    let link_color = style::bubble_link(c, mine);
    let code_bg = style::bubble_tint(c, mine, 0.12);
    let spans: Vec<iced::widget::text::Span<'a, String>> = spans
        .iter()
        .map(|s| {
            let st = s.style;
            let mut font = if st.code {
                Font::MONOSPACE
            } else {
                Font::DEFAULT
            };
            if st.bold || bold {
                font.weight = iced::font::Weight::Bold;
            } else if st.mention {
                font.weight = iced::font::Weight::Semibold;
            }
            if st.italic {
                font.style = iced::font::Style::Italic;
            }
            let mut sp = span(s.text.clone())
                .font(font)
                .underline(st.underline || s.link.is_some())
                .strikethrough(st.strike);
            if st.code {
                sp = sp.background(code_bg).padding([0, 3]);
            }
            if st.mention {
                sp = sp.color(link_color);
            }
            if let Some(href) = &s.link {
                sp = sp.color(link_color).link(href.clone());
            }
            sp
        })
        .collect();
    rich_text(spans)
        .on_link_click(Msg::OpenUrl)
        .size(size)
        .line_height(1.35)
        .into()
}

/// Obrázek v bublině; velikost podle rozměrů z HTML, zmenšená na [`IMAGE_MAX_WIDTH`].
fn image_view<'a>(img: &InlineImage, state: Option<&Media>) -> Element<'a, Msg> {
    let (w, h) = match (img.width, img.height) {
        (Some(w), Some(h)) => {
            let w2 = w.min(IMAGE_MAX_WIDTH);
            (w2, h * w2 / w)
        }
        _ => (IMAGE_MAX_WIDTH, IMAGE_MAX_WIDTH * 0.75),
    };
    match state {
        Some(Media::Ready(handle, _)) => {
            button(image(handle.clone()).width(w).height(h).border_radius(10))
                .style(button::text)
                .padding(0)
                .on_press(Msg::OpenImage(img.clone()))
                .into()
        }
        Some(Media::Failed) => text("🖼 Obrázek se nepodařilo načíst")
            .size(13)
            .style(muted)
            .into(),
        _ => container(text("Načítám obrázek…").size(12).style(muted))
            .width(w)
            .height(h)
            .center_x(w)
            .center_y(h)
            .style(style::day_pill)
            .into(),
    }
}

/// „Dnes“, „Včera“, jinak datum.
fn day_label(d: chrono::NaiveDate) -> String {
    let today = Local::now().date_naive();
    if d == today {
        "Dnes".into()
    } else if Some(d) == today.pred_opt() {
        "Včera".into()
    } else {
        d.format("%-d. %-m. %Y").to_string()
    }
}

/// Text pro náhled v seznamu chatů; obrázky jako „🖼 Obrázek“.
/// Citace odpovědi (náhled cizí zprávy) se vynechá.
fn preview_text(html: &str) -> String {
    let parts: Vec<String> = rich::parse(html)
        .iter()
        .filter_map(|b| match b {
            Block::Image(_) => Some("🖼 Obrázek".into()),
            Block::Quote(q) if matches!(q.kind, QuoteKind::Reply { .. }) => None,
            b => Some(rich::plain(std::slice::from_ref(b)).replace('\n', " ")),
        })
        .filter(|t| !t.is_empty())
        .collect();
    parts.join(" ")
}

/// Zapíše dočasnou kopii média pro otevření v externí aplikaci.
fn write_temp_media(url: &str, bytes: &[u8]) -> std::io::Result<std::path::PathBuf> {
    use std::hash::{Hash, Hasher};
    use std::os::unix::fs::PermissionsExt;
    let ext = match bytes {
        [0xFF, 0xD8, ..] => "jpg",
        [0x89, b'P', b'N', b'G', ..] => "png",
        [b'G', b'I', b'F', ..] => "gif",
        [
            b'R',
            b'I',
            b'F',
            b'F',
            _,
            _,
            _,
            _,
            b'W',
            b'E',
            b'B',
            b'P',
            ..,
        ] => "webp",
        _ => "bin",
    };
    let dir = temp_media_dir()?;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    url.hash(&mut h);
    let path = dir.join(format!("{:016x}.{ext}", h.finish()));
    std::fs::write(&path, bytes)?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    Ok(path)
}

/// Adresář pro dočasné kopie médií pro externí prohlížeč: `$XDG_RUNTIME_DIR/tymy/media`
/// (paměťový disk jen pro uživatele, mazaný při odhlášení). Trvalá kopie je v šifrované cache.
fn temp_media_dir() -> std::io::Result<std::path::PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let dir = base.join("tymy").join("media");
    std::fs::create_dir_all(&dir)?;
    for d in [base.join("tymy"), dir.clone()] {
        std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(dir)
}

/// Starší verze ukládaly obrázky nešifrovaně do `~/.cache/tymy/media`; smazat.
fn remove_legacy_media_cache() {
    if let Some(dirs) = directories::ProjectDirs::from("", "", "tymy") {
        let legacy = dirs.cache_dir().join("media");
        if legacy.exists()
            && let Err(e) = std::fs::remove_dir_all(&legacy)
        {
            tracing::warn!(error = %e, "nelze smazat starou cache obrázků");
        }
    }
}

/// Otevře soubor ve výchozí aplikaci.
fn open_path(path: &std::path::Path) {
    if let Err(e) = std::process::Command::new("xdg-open").arg(path).spawn() {
        tracing::warn!(error = %e, "xdg-open selhal");
    }
}

fn centered<'a>(content: impl Into<Element<'a, Msg>>) -> Element<'a, Msg> {
    container(content)
        .center_x(Length::Fill)
        .center_y(Length::Fill)
        .into()
}

fn error_banner(err: &str) -> Element<'_, Msg> {
    container(
        row![
            text("⚠").size(14).style(style::danger),
            text(err).size(13).style(style::danger),
            space::horizontal(),
            button(text("✕").size(12))
                .padding([2, 8])
                .style(style::ghost)
                .on_press(Msg::DismissError),
        ]
        .spacing(10)
        .align_y(iced::Alignment::Center),
    )
    .padding([8, 16])
    .width(Length::Fill)
    .style(style::error_banner)
    .into()
}

fn short_time(t: Option<DateTime<Utc>>) -> String {
    let Some(t) = t else { return String::new() };
    let t = t.with_timezone(&Local);
    if t.date_naive() == Local::now().date_naive() {
        t.format("%H:%M").to_string()
    } else {
        t.format("%-d. %-m.").to_string()
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() > max {
        format!("{}…", s.chars().take(max - 1).collect::<String>())
    } else {
        s.to_owned()
    }
}

/// Otevře odkaz v prohlížeči; jen web a e-mail (žádné `file:` ani vlastní schémata).
fn open_url(url: &str) {
    if !rich::safe_link(url) {
        tracing::warn!(%url, "odmítám otevřít odkaz s nepovoleným schématem");
        return;
    }
    if let Err(e) = std::process::Command::new("xdg-open").arg(url).spawn() {
        tracing::warn!(error = %e, "xdg-open selhal");
    }
}

/// Účty z konfigurace (pro menu účtů).
fn load_accounts() -> Vec<AccountRecord> {
    Config::load()
        .map(|c| c.accounts.accounts)
        .unwrap_or_default()
}

/// Připraví relaci účtu `id` (bez něj naposledy použitého) a zapamatuje si ho jako aktivní.
async fn load_session(id: Option<AccountId>) -> Result<Option<Session>, Failure> {
    let mut config = Config::load()?;
    if let Some(id) = id
        && config.settings.account.as_ref() != Some(&id)
    {
        config.settings.account = Some(id);
        if let Err(e) = config.save_settings() {
            tracing::warn!(error = %e, "uložení aktivního účtu");
        }
    }
    let Some(record) = config.active().cloned() else {
        return Ok(None);
    };
    let ep = config.endpoints(record.kind, record.authority.as_deref());
    let store = SecretServiceStore::new().await?;
    let profile = record.profile();
    remove_legacy_media_cache();
    let key = match store.cache_key().await {
        Ok(k) => Some(k),
        Err(e) => {
            tracing::warn!(error = %e, "klíč cache není k dispozici, pokračuji bez cache");
            None
        }
    };
    let backend = TeamsBackend::new(profile.clone(), ep, Http::new()?, Arc::new(store));
    let cache = key
        .zip(directories::ProjectDirs::from("", "", "tymy").map(|d| d.data_dir().join("cache.sqlite")))
        .and_then(|(key, path)| match Store::open(&path, &key) {
            Ok(s) => {
                if let Err(e) = s.prune_media(MEDIA_CACHE_LIMIT) {
                    tracing::warn!(error = %e, "úklid cache obrázků");
                }
                Some(s)
            }
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "cache nelze otevřít, pokračuji bez ní");
                None
            }
        });
    Ok(Some(Session {
        profile,
        backend: Arc::new(backend),
        store: cache,
    }))
}

async fn start_login(kind: AccountKind) -> Result<DeviceCode, Failure> {
    let config = Config::load()?;
    let ep = config.endpoints(kind, None);
    Ok(auth::start_device_code(&Http::new()?, &ep).await?)
}

async fn finish_login(kind: AccountKind, code: DeviceCode) -> Result<AccountId, Failure> {
    let ep = Config::load()?.endpoints(kind, None);
    let tokens = auth::poll_device_code(&Http::new()?, &ep, &code).await?;
    save_login(kind, tokens).await
}

async fn finish_browser_login(
    kind: AccountKind,
    login: BrowserLogin,
    code: String,
) -> Result<AccountId, Failure> {
    let ep = Config::load()?.endpoints(kind, None);
    let tokens = auth::redeem_code(&Http::new()?, &ep, &login, &code).await?;
    save_login(kind, tokens).await
}

/// Uloží refresh token do Secret Service a účet do konfigurace.
async fn save_login(kind: AccountKind, tokens: auth::TokenSet) -> Result<AccountId, Failure> {
    let mut config = Config::load()?;
    let store = SecretServiceStore::new().await?;
    let profile = complete_login(kind, &tokens, &store).await?;
    config.upsert(AccountRecord::from_profile(&profile, None));
    config.save()?;
    Ok(profile.account_id)
}

/// Jména účastníků chatů z profilů. Vrátí (chat, jména) a chaty, u kterých to nevyšlo.
async fn resolve_names(
    backend: Arc<TeamsBackend>,
    me: AccountId,
    todo: Vec<(ConversationId, ConversationKind)>,
) -> (Vec<(ConversationId, Vec<String>)>, Vec<ConversationId>) {
    let mut members: Vec<(ConversationId, Vec<tymy_core::UserId>)> = vec![];
    let mut unresolved = vec![];
    let mut groups = 0;
    for (id, kind) in todo {
        match kind {
            ConversationKind::OneOnOne => match one_on_one_peer(&id, me.as_str()) {
                Some(peer) => members.push((id, vec![peer])),
                None => unresolved.push(id),
            },
            _ if groups < GROUP_MEMBERS_LIMIT => {
                groups += 1;
                match backend.members(&id).await {
                    Ok(m) => {
                        let others: Vec<_> = m
                            .into_iter()
                            .filter(|u| !u.as_str().ends_with(me.as_str()))
                            .collect();
                        members.push((id, others));
                    }
                    Err(e) => {
                        tracing::debug!(%id, error = %e, "členové skupinového chatu");
                        unresolved.push(id);
                    }
                }
            }
            _ => unresolved.push(id),
        }
    }
    let mut all: Vec<tymy_core::UserId> = members.iter().flat_map(|(_, m)| m.clone()).collect();
    all.sort_by(|a, b| a.as_str().cmp(b.as_str()));
    all.dedup();
    let names: HashMap<String, String> = match backend.profiles(&all).await {
        Ok(people) => people
            .into_iter()
            .map(|p| (p.mri.as_str().to_owned(), p.display_name))
            .collect(),
        Err(e) => {
            tracing::warn!(error = %e, "profily účastníků chatů");
            unresolved.extend(members.into_iter().map(|(id, _)| id));
            return (vec![], unresolved);
        }
    };
    let mut resolved = vec![];
    for (id, m) in members {
        let found: Vec<String> = m
            .iter()
            .filter_map(|u| names.get(u.as_str()).cloned())
            .collect();
        if found.is_empty() {
            unresolved.push(id);
        } else {
            resolved.push((id, found));
        }
    }
    tracing::info!(
        resolved = resolved.len(),
        unresolved = unresolved.len(),
        "jména účastníků z profilů"
    );
    (resolved, unresolved)
}

/// Počet nepřečtených konverzací neaktivního účtu (spojení se vytvoří při prvním dotazu
/// a pak se používá znovu, ať se zbytečně neobnovují tokeny).
async fn other_unread(
    record: AccountRecord,
    backend: Option<Arc<TeamsBackend>>,
) -> Result<(OtherBackend, Vec<Conversation>), Failure> {
    let backend = match backend {
        Some(b) => b,
        None => {
            let ep = Config::load()?.endpoints(record.kind, record.authority.as_deref());
            let store = SecretServiceStore::new().await?;
            Arc::new(TeamsBackend::new(
                record.profile(),
                ep,
                Http::new()?,
                Arc::new(store),
            ))
        }
    };
    // nepřečtené a nové zprávy jsou v nejnověji aktivních chatech
    let convs = backend.recent_conversations().await?;
    Ok((OtherBackend(backend), convs))
}

/// Poslední zpráva každého chatu (id v ms) pro porovnání při dalším dotazu.
fn last_seen(convs: &[Conversation]) -> HashMap<ConversationId, u64> {
    convs
        .iter()
        .filter(|c| c.is_chat())
        .filter_map(|c| Some((c.id.clone(), msg_num(&c.last_message.as_ref()?.id))))
        .collect()
}

/// Notifikace o nových zprávách v chatech neaktivního účtu (od minulého dotazu).
fn notify_other(
    record: &AccountRecord,
    me: &Profile,
    convs: &[Conversation],
    previous: &HashMap<ConversationId, u64>,
) -> Task<Msg> {
    let fresh: Vec<(&Conversation, &Message)> = convs
        .iter()
        .filter(|c| c.is_chat())
        .filter_map(|c| {
            let m = c.last_message.as_ref()?;
            let newer = previous.get(&c.id).is_none_or(|p| msg_num(&m.id) > *p);
            (newer && m.is_user_content() && !m.deleted && !is_me(me, m)).then_some((c, m))
        })
        .collect();
    let account = record
        .display_name
        .clone()
        .or_else(|| record.username.clone())
        .unwrap_or_else(|| "jiný účet".into());
    if fresh.is_empty() {
        return Task::none();
    }
    if fresh.len() > MAX_NOTIFICATIONS {
        let n = fresh.len();
        return Task::perform(
            notify::show(
                format!("Tymy · {account}"),
                format!("Nové zprávy v {n} chatech"),
            ),
            |_| Msg::Noop,
        );
    }
    Task::batch(fresh.into_iter().map(|(c, m)| {
        let who = m.sender_name.clone().unwrap_or_default();
        let title = match c.topic.as_deref().filter(|t| !t.trim().is_empty()) {
            Some(t) => t.to_owned(),
            None if !who.is_empty() => who.clone(),
            None => "Chat".into(),
        };
        let text = truncate(&preview_text(&m.content), 200);
        let body = if c.topic.is_none() || who.is_empty() {
            text
        } else {
            format!("{who}: {text}")
        };
        let (account_id, conv) = (record.id.clone(), c.id.clone());
        Task::perform(
            notify::show(format!("{title} · {account}"), body),
            move |clicked| {
                if clicked {
                    Msg::OtherNotificationClicked(account_id.clone(), conv.clone())
                } else {
                    Msg::Noop
                }
            },
        )
    }))
}

/// Smaže přihlášení účtu ze Secret Service a odebere ho z konfigurace.
async fn sign_out(id: AccountId) -> Result<(), Failure> {
    let store = SecretServiceStore::new().await?;
    store.delete(&id).await?;
    let mut config = Config::load()?;
    config.remove(&id);
    config.save()?;
    config.save_settings()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_columns_fit_content_and_bubble() {
        let cell = |t: &str| rich::Cell {
            header: false,
            spans: vec![rich::Span {
                text: t.into(),
                style: Default::default(),
                link: None,
            }],
        };
        let rows = vec![
            vec![cell("1"), cell("Krátký"), cell(&"dlouhý text ".repeat(20))],
            vec![cell("22"), cell("Delší buňka"), cell("x")],
        ];
        // dost místa: každý sloupec podle svého nejdelšího textu
        let wide = table_columns(&rows, 5000.0);
        assert_eq!(wide[0], 40.0, "úzký sloupec má minimum");
        assert!(wide[2] > 1000.0);
        // málo místa: úzké sloupce zůstanou, zúží se jen ten široký
        let tight = table_columns(&rows, 600.0);
        assert_eq!(tight[0], wide[0]);
        assert_eq!(tight[1], wide[1]);
        assert!((tight.iter().sum::<f32>() - 600.0).abs() < 0.5, "{tight:?}");
    }

    #[test]
    fn plain_text_for_copy_and_width() {
        let m = Message {
            id: "1".into(),
            conversation_id: "c".into(),
            client_message_id: None,
            sender: None,
            sender_name: None,
            kind: MessageKind::RichText,
            content: r#"<p>Ahoj <b>světe</b></p><img src="https://eu-api.asm.skype.com/v1/objects/x/views/imgo" itemtype="http://schema.skype.com/AMSImage"><p>druhý</p>"#.into(),
            composed_at: None,
            arrived_at: None,
            edited_at: None,
            deleted: false,
            attachments: vec![tymy_core::Attachment {
                name: "soubor.pdf".into(),
                url: None,
                size: None,
                file_type: None,
            }],
            reactions: vec![],
        };
        assert_eq!(message_plain(&m), "Ahoj světe\ndruhý\n\nsoubor.pdf");
        assert_eq!(estimate_text_width("ok", 500.0), 80.0);
        assert_eq!(estimate_text_width(&"x".repeat(500), 500.0), 500.0);
    }

    #[test]
    fn truncate_counts_chars() {
        assert_eq!(truncate("žluťoučký", 5), "žluť…");
        assert_eq!(truncate("krátký", 10), "krátký");
    }
}
