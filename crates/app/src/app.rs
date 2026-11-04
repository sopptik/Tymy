//! Hlavní okno: seznam chatů vlevo, vlákno zpráv vpravo (rozložení jako Skype).

use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Local, Utc};
use iced::widget::{
    Column, Id, button, column, container, image, mouse_area, operation, rich_text, row,
    scrollable, space, span, stack, text, text_editor, text_input,
};
use iced::{Border, Color, Element, Font, Length, Subscription, Task, Theme};
use tymy_api::auth::{self, DeviceCode, SecretServiceStore};
use tymy_api::{Http, TeamsBackend, complete_login};
use tymy_config::{AccountRecord, Config, ThemeMode};
use tymy_core::text::rich::{self, Block, QuoteKind};
use tymy_core::text::{
    InlineImage, Part, REPLY_PREVIEW_CHARS, edited_html, message_parts, truncate_chars,
};
use tymy_core::{
    AccountId, AccountKind, ChatBackend, Conversation, ConversationId, ConversationKind, Error,
    Message, MessageId, MessageKind, MessagePage, Outgoing, OutgoingImage, Person, Profile,
    ReplyRef, one_on_one_id,
};

use tymy_store::{HIT_END, HIT_START, SearchHit, Store};

use iced::widget::text::Wrapping;

use crate::clipboard::{self, Clip};
use crate::notify;
use crate::state::{
    PeerNames, channel_name, group_teams, is_me, merge_messages, move_pin, my_mri, order_chats,
    toggle_reaction,
};
use crate::style::{self, BOLD, SEMIBOLD, accent, muted};
use crate::tray::{self, TrayEvent, TrayHandle};

const COMPOSER: Id = Id::new("composer");
const MESSAGES: Id = Id::new("messages");
/// Dokud nemáme trouter (WebSocket), obnovujeme seznam chatů periodicky.
const REFRESH: Duration = Duration::from_secs(5);
/// Kolik chatů bez známého jména projít při startu kvůli jménům účastníků.
const PEER_SCAN_LIMIT: usize = 25;

/// Chyba převedená pro UI; `reauth` = je nutné se znovu přihlásit.
#[derive(Debug, Clone)]
pub struct Failure {
    text: String,
    reauth: bool,
    /// Server odpověděl 404 (např. vlákno nového chatu ještě neexistuje).
    not_found: bool,
}

impl From<Error> for Failure {
    fn from(e: Error) -> Self {
        Self {
            reauth: matches!(e, Error::ReauthRequired(_)),
            not_found: matches!(e, Error::Http { status: 404, .. }),
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

#[derive(Debug, Clone)]
pub enum Msg {
    Booted(Result<Option<Session>, Failure>),
    StartLogin(AccountKind),
    LoginCode(AccountKind, Result<DeviceCode, Failure>),
    LoginDone(Result<(), Failure>),
    OpenUrl(String),
    Tick,
    Conversations(Result<Vec<Conversation>, Failure>),
    PeersScanned(Vec<Message>),
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
    /// Ladicí záznam o události: název a stručná metadata, nikdy obsah zpráv.
    fn log(&self) {
        use tracing::{debug, trace};
        let ok = |r: bool| if r { "ok" } else { "chyba" };
        match self {
            // časté a nezajímavé
            Msg::Scrolled(_) | Msg::Compose(_) | Msg::Search(_) => {
                trace!(event = ?std::mem::discriminant(self), "update")
            }
            Msg::Tick => trace!("update: Tick"),
            Msg::Booted(r) => debug!(
                result = ok(r.is_ok()),
                account = matches!(r, Ok(Some(_))),
                "update: Booted"
            ),
            Msg::StartLogin(kind) => debug!(%kind, "update: StartLogin"),
            Msg::LoginCode(kind, r) => debug!(%kind, result = ok(r.is_ok()), "update: LoginCode"),
            Msg::LoginDone(r) => debug!(result = ok(r.is_ok()), "update: LoginDone"),
            Msg::OpenUrl(_) => debug!("update: OpenUrl"),
            Msg::Conversations(r) => debug!(
                result = ok(r.is_ok()),
                count = r.as_ref().map(Vec::len).unwrap_or(0),
                "update: Conversations"
            ),
            Msg::PeersScanned(m) => debug!(messages = m.len(), "update: PeersScanned"),
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
}

enum Phase {
    Starting,
    Login {
        code: Option<DeviceCode>,
        busy: bool,
    },
    Ready(Session),
}

pub struct App {
    phase: Phase,
    conversations: Vec<Conversation>,
    peers: PeerNames,
    /// Zprávy podle konverzace, od nejstarší.
    messages: HashMap<ConversationId, Vec<Message>>,
    /// Kurzor na starší historii; `None` v mapě = ještě nenačteno, `Some(None)` = začátek.
    older: HashMap<ConversationId, Option<String>>,
    /// Co jsme přečetli v tomto okně (serveru to zatím nehlásíme).
    read_locally: HashMap<ConversationId, MessageId>,
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
    /// Obrázky ze zpráv podle URL.
    media: HashMap<String, Media>,
    tab: Tab,
    /// Obrázky vložené do psacího pole, čekající na odeslání.
    pending_images: Vec<PendingImage>,
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
    /// Notifikace až po prvním obnovení ze sítě.
    notify_ready: bool,
    tray: Option<TrayHandle>,
    /// Počet nepřečtených, který ikona v liště naposledy ukazovala.
    tray_unread: usize,
    /// Okno je skryté do lišty.
    hidden: bool,
    /// Ručně seřazené připnuté chaty (první = nahoře).
    pins: Vec<ConversationId>,
    hovered: Option<ConversationId>,
    /// Originály obrázků uložené na disk (URL originálu → soubor).
    originals: HashMap<String, std::path::PathBuf>,
}

impl App {
    pub fn boot() -> (Self, Task<Msg>) {
        let app = Self {
            phase: Phase::Starting,
            conversations: vec![],
            peers: PeerNames::default(),
            messages: HashMap::new(),
            older: HashMap::new(),
            read_locally: HashMap::new(),
            selected: None,
            search: String::new(),
            composer: text_editor::Content::new(),
            sending: false,
            refreshing: false,
            loading_older: false,
            at_bottom: true,
            new_below: false,
            peer_scan_done: false,
            fresh: Default::default(),
            error: None,
            media: HashMap::new(),
            tab: Tab::Chats,
            pending_images: vec![],
            in_flight_images: vec![],
            replying: None,
            in_flight_reply: None,
            editing: None,
            reacting: None,
            confirm_delete: None,
            highlight: None,
            message_hits: vec![],
            older_in_store: HashSet::new(),
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
            notify_ready: false,
            tray: None,
            tray_unread: 0,
            hidden: false,
            pins: vec![],
            hovered: None,
            search_seq: 0,
            people: vec![],
            searching_people: false,
            people_note: None,
            draft_chats: vec![],
            draft_people: HashMap::new(),
            originals: HashMap::new(),
        };
        let system_theme = iced::system::theme().map(Msg::SystemTheme);
        (
            app,
            Task::batch([Task::perform(load_session(), Msg::Booted), system_theme]),
        )
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
            _ => None,
        });
        let always = [
            focus,
            iced::system::theme_changes().map(Msg::SystemTheme),
            tray::subscription(),
            iced::window::close_requests().map(Msg::CloseRequested),
        ];
        match self.phase {
            Phase::Ready(_) => Subscription::batch(
                always
                    .into_iter()
                    .chain([iced::time::every(REFRESH).map(|_| Msg::Tick)]),
            ),
            _ => Subscription::batch(always),
        }
    }

    fn session(&self) -> Option<&Session> {
        match &self.phase {
            Phase::Ready(s) => Some(s),
            _ => None,
        }
    }

    fn fail(&mut self, f: Failure) -> Task<Msg> {
        tracing::error!(error = %f.text, reauth = f.reauth, "chyba");
        if f.reauth {
            self.phase = Phase::Login {
                code: None,
                busy: false,
            };
        }
        self.error = Some(f.text);
        Task::none()
    }

    fn unread(&self, c: &Conversation) -> bool {
        let Some(last) = &c.last_message else {
            return false;
        };
        let n = |id: &MessageId| id.as_str().parse::<u64>().unwrap_or(0);
        let read = c
            .read_up_to
            .iter()
            .chain(self.read_locally.get(&c.id))
            .map(n)
            .max()
            .unwrap_or(0);
        let mine = self.session().is_some_and(|s| is_me(&s.profile, last));
        !mine && last.is_user_content() && n(&last.id) > read
    }

    fn fetch_messages(&self, id: ConversationId, cursor: Option<String>) -> Task<Msg> {
        let Some(s) = self.session() else {
            return Task::none();
        };
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
        Task::perform(
            async move { backend.conversations().await.map_err(Failure::from) },
            Msg::Conversations,
        )
    }

    fn mark_read(&mut self, id: &ConversationId) {
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
        self.toast = Some(toast.to_owned());
        self.toast_seq += 1;
        let seq = self.toast_seq;
        Task::batch([
            iced::clipboard::write(text.clone()),
            iced::clipboard::write_primary(text),
            Task::perform(tokio::time::sleep(TOAST_DURATION), move |_| {
                Msg::HideToast(seq)
            }),
        ])
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
            .filter(|c| !self.is_draft(&c.id))
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
        let task = self.handle(msg);
        Task::batch([task, self.sync_tray()])
    }

    /// Aktualizuje počet nepřečtených v liště, pokud se změnil.
    fn sync_tray(&mut self) -> Task<Msg> {
        let Some(tray) = self.tray.clone() else {
            return Task::none();
        };
        let n = self.conversations.iter().filter(|c| self.unread(c)).count();
        if n == self.tray_unread {
            return Task::none();
        }
        self.tray_unread = n;
        Task::perform(tray.set_unread(n), |_| Msg::Noop)
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
                self.load_cached(&session);
                self.phase = Phase::Ready(session);
                self.fetch_conversations()
            }
            Msg::Booted(Ok(None)) => {
                self.phase = Phase::Login {
                    code: None,
                    busy: false,
                };
                Task::none()
            }
            Msg::Booted(Err(e)) => self.fail(e),

            Msg::StartLogin(kind) => {
                self.phase = Phase::Login {
                    code: None,
                    busy: true,
                };
                self.error = None;
                Task::perform(start_login(kind), move |r| Msg::LoginCode(kind, r))
            }
            Msg::LoginCode(kind, Ok(code)) => {
                self.phase = Phase::Login {
                    code: Some(code.clone()),
                    busy: true,
                };
                Task::perform(finish_login(kind, code), Msg::LoginDone)
            }
            Msg::LoginCode(_, Err(e)) | Msg::LoginDone(Err(e)) => {
                self.phase = Phase::Login {
                    code: None,
                    busy: false,
                };
                self.fail(e)
            }
            Msg::LoginDone(Ok(())) => {
                self.phase = Phase::Starting;
                Task::perform(load_session(), Msg::Booted)
            }

            Msg::OpenUrl(url) => {
                open_url(&url);
                Task::none()
            }

            Msg::Tick => {
                if self.refreshing {
                    Task::none()
                } else {
                    self.fetch_conversations()
                }
            }
            Msg::Conversations(Ok(list)) => {
                self.refreshing = false;
                let Some(session) = self.session().cloned() else {
                    return Task::none();
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
                        }
                    }
                    // ladění: `TYMY_SEARCH=<dotaz>` vyplní hledání
                    if let Ok(q) = std::env::var("TYMY_SEARCH") {
                        tasks.push(Task::done(Msg::Search(q)));
                    }
                    let ids: Vec<_> = self
                        .conversations
                        .iter()
                        .filter(|c| c.is_chat() && c.topic.is_none() && !self.peers.knows(&c.id))
                        .take(PEER_SCAN_LIMIT)
                        .map(|c| c.id.clone())
                        .collect();
                    let backend = session.backend.clone();
                    tasks.push(Task::perform(
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
                    ));
                }
                Task::batch(tasks)
            }
            Msg::Conversations(Err(e)) => {
                self.refreshing = false;
                self.fail(e)
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
                if self.selected.as_ref() != Some(&id) {
                    self.replying = None;
                    self.cancel_edit();
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

            Msg::Search(s) => {
                self.search = s;
                self.search_seq += 1;
                self.people_note = None;
                if self.tab != Tab::Chats {
                    self.people.clear();
                    self.searching_people = false;
                }
                if self.search.trim().chars().count() < MIN_PEOPLE_QUERY {
                    self.people.clear();
                    self.searching_people = false;
                    self.message_hits.clear();
                    return Task::none();
                }
                let seq = self.search_seq;
                Task::perform(tokio::time::sleep(SEARCH_DEBOUNCE), move |_| {
                    Msg::SearchFire(seq)
                })
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
            Msg::WindowFocus(f) => {
                self.focused = f;
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
                Task::none()
            }
            Msg::Tray(TrayEvent::Show) => self.show_window(),
            Msg::Tray(TrayEvent::Quit) => iced::exit(),
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
                let width = estimate_text_width(&text);
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
                let text = rich::plain(&rich::parse(&m.content));
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
            Msg::OpenHit(conv, id) => self.open_hit(conv, id),
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
                Task::none()
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
                });
                // U nového chatu: když vlákno neexistuje, založit ho a poslat znovu.
                let draft_peer = self.draft_people.get(&id).map(|p| p.mri.clone());
                Task::perform(
                    {
                        let id = id.clone();
                        async move {
                            let send = |to: ConversationId| {
                                let (backend, outgoing) = (backend.clone(), outgoing.clone());
                                async move { backend.send(&to, &outgoing).await }
                            };
                            match send(id.clone()).await {
                                Ok(_) => Ok(id),
                                Err(Error::Http { status: 404, .. }) if draft_peer.is_some() => {
                                    let peer = draft_peer.expect("ověřeno výše");
                                    tracing::info!(%id, "vlákno neexistuje, zakládám chat");
                                    let created = backend.create_chat(&[peer]).await?;
                                    send(created.clone()).await?;
                                    Ok(created)
                                }
                                Err(e) => Err(e),
                            }
                            .map_err(Failure::from)
                        }
                    },
                    move |r| Msg::Sent(id.clone(), text.clone(), r),
                )
            }
            Msg::Sent(requested, _, Ok(id)) => {
                self.sending = false;
                self.in_flight_images.clear();
                self.in_flight_reply = None;
                if id != requested {
                    // nový chat dostal od serveru jiné id než odhadnuté
                    if let Some(person) = self.draft_people.remove(&requested) {
                        self.peers.set(&id, &person.display_name);
                    }
                    self.draft_chats.retain(|d| d.id != requested);
                    self.conversations.retain(|c| c.id != requested);
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
            Msg::Sent(_, text, Err(e)) => {
                self.sending = false;
                // vrátit i obrázky
                let mut back = std::mem::take(&mut self.in_flight_images);
                back.append(&mut self.pending_images);
                self.pending_images = back;
                if self.replying.is_none() {
                    self.replying = self.in_flight_reply.take();
                }
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
            Phase::Login { code, busy } => self.login_view(code.as_ref(), *busy),
            Phase::Ready(session) => {
                row![self.sidebar(session), vline(), self.chat_pane(session),].into()
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

    fn login_view(&self, code: Option<&DeviceCode>, busy: bool) -> Element<'_, Msg> {
        let content: Element<'_, Msg> = match code {
            Some(code) => column![
                text("Přihlášení").size(22).font(BOLD),
                text("1. Otevřete přihlašovací stránku Microsoftu").style(muted),
                button(text(code.verification_uri.clone()).size(14))
                    .padding(0)
                    .style(|t: &Theme, s| style::link(t, s, false))
                    .on_press(Msg::OpenUrl(code.verification_uri.clone())),
                space().height(4),
                text("2. Zadejte tento kód").style(muted),
                container(text(code.user_code.clone()).size(28).font(Font::MONOSPACE))
                    .padding([10, 18])
                    .style(style::day_pill),
                space().height(4),
                text("3. Potvrďte přihlášení. Okno pak pokračuje samo.")
                    .size(13)
                    .style(muted),
            ]
            .spacing(10)
            .into(),
            None => column![
                row![logo(40.0), text("Tymy").size(30).font(BOLD)]
                    .spacing(12)
                    .align_y(iced::Alignment::Center),
                text("Lehký klient Teams pro Linux").style(muted),
                space().height(18),
                button(
                    text("Přihlásit pracovní / školní účet")
                        .center()
                        .width(Length::Fill)
                )
                .width(Length::Fill)
                .padding([11, 20])
                .style(style::primary)
                .on_press_maybe((!busy).then_some(Msg::StartLogin(AccountKind::Work))),
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
        centered(
            container(content)
                .padding(32)
                .max_width(440)
                .style(style::card),
        )
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
        let profile = row![
            avatar(me, 34.0),
            column![
                text(truncate(me, 22)).size(14).font(SEMIBOLD),
                text(truncate(
                    session.profile.username.as_deref().unwrap_or_default(),
                    28
                ))
                .size(11)
                .style(muted),
            ]
            .spacing(1),
            space::horizontal(),
            icon_button(theme_icon, Some(Msg::ToggleTheme), theme_tip),
        ]
        .spacing(10)
        .align_y(iced::Alignment::Center);
        let tabs = container(
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
        )
        .padding(3)
        .style(style::segmented);
        let (items, placeholder) = match self.tab {
            Tab::Chats => (self.chat_list(session, &query), "Hledat chat nebo osobu…"),
            Tab::Teams => (self.team_list(&query), "Hledat tým nebo kanál…"),
        };
        container(column![
            container(profile).padding(iced::Padding {
                top: 14.0,
                right: 12.0,
                bottom: 12.0,
                left: 14.0
            }),
            container(tabs).padding([0, 12]),
            container(
                text_input(placeholder, &self.search)
                    .on_input(Msg::Search)
                    .padding([8, 12])
                    .size(13)
                    .style(style::search_input)
            )
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
            .height(Length::Fill),
        ])
        .width(SIDEBAR_WIDTH)
        .height(Length::Fill)
        .style(style::sidebar)
        .into()
    }

    /// Záložka Chaty: připnuté v ručním pořadí, ostatní podle poslední komunikace, pak lidé.
    fn chat_list<'a>(&'a self, session: &'a Session, query: &str) -> Vec<Element<'a, Msg>> {
        let matches = |c: &&Conversation| {
            c.is_chat() && (query.is_empty() || self.peers.title(c).to_lowercase().contains(query))
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
    fn team_list<'a>(&'a self, query: &str) -> Vec<Element<'a, Msg>> {
        let mut items: Vec<Element<'a, Msg>> = vec![];
        let channels = self
            .conversations
            .iter()
            .filter(|c| c.kind == ConversationKind::Channel);
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
                .style(muted),
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

    fn chat_pane<'a>(&'a self, session: &'a Session) -> Element<'a, Msg> {
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
        let header = container(
            row![
                header_avatar,
                column![
                    text(title.clone()).size(16).font(BOLD),
                    text(subtitle).size(12).style(muted),
                ]
                .spacing(1),
            ]
            .spacing(12)
            .align_y(iced::Alignment::Center),
        )
        .padding([12, 20])
        .width(Length::Fill)
        .style(style::header);

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
                "Enter uloží, Esc zruší. Formátování se nahradí prostým textem.".into(),
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
        let composer = container(
            container(column![
                banner,
                attachments,
                row![
                    text_editor(&self.composer)
                        .id(COMPOSER)
                        .placeholder("Napište zprávu…  (Shift+Enter = nový řádek)")
                        .on_action(Msg::Compose)
                        .key_binding({
                            let remove_last =
                                self.composer.text().trim_end_matches('\n').is_empty()
                                    && !self.pending_images.is_empty();
                            let keys = ComposerKeys {
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
}

/// Prostý text zprávy pro kopírování (bez obrázků; přílohy jako názvy).
fn message_plain(m: &Message) -> String {
    let mut parts: Vec<String> = vec![rich::plain(&rich::parse(&m.content))];
    parts.retain(|p| !p.is_empty());
    parts.extend(m.attachments.iter().map(|a| a.name.clone()));
    parts.join("\n\n")
}

/// Odhad šířky textu (px) pro editor v režimu výběru; raději o něco širší.
fn estimate_text_width(text: &str) -> f32 {
    let longest = text.lines().map(|l| l.chars().count()).max().unwrap_or(0) as f32;
    (longest * 8.2 + 8.0).clamp(80.0, BUBBLE_TEXT_MAX)
}

/// Maximální šířka textu v bublině (bublina má 560 px včetně odsazení).
const BUBBLE_TEXT_MAX: f32 = 534.0;

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
                ..style::colors(t).on_primary
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
            .max_width(560)
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
) -> Vec<Element<'a, Msg>> {
    blocks
        .iter()
        .map(|b| block_view(b, mine, quoted, media, c))
        .collect()
}

fn block_view<'a>(
    block: &Block,
    mine: bool,
    quoted: bool,
    media: &'a HashMap<String, Media>,
    c: &style::Colors,
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
            let inner = blocks_view(&q.blocks, mine, true, media, c);
            col = col.extend(inner);
            container(
                container(col)
                    .padding([5, 10])
                    .style(move |t: &Theme| style::quote(t, mine)),
            )
            .padding(iced::Padding {
                left: 3.0,
                ..iced::Padding::ZERO
            })
            .style(move |t: &Theme| style::quote_frame(t, mine))
            .into()
        }
        Block::Table(rows) => {
            let table = Column::with_children(rows.iter().map(|r| {
                iced::widget::Row::with_children(r.iter().map(|cell| {
                    let header = cell.header;
                    container(paragraph(&cell.spans, 13.0, header, mine, c))
                        .padding([4, 8])
                        .width(Length::Fill)
                        .style(move |t: &Theme| style::table_cell(t, mine, header))
                        .into()
                }))
                .into()
            }));
            // pevná šířka: s `Fill` by se tabulka dělila o místo s odsazením bubliny
            container(table).width(BUBBLE_TEXT_MAX).into()
        }
        Block::Image(img) => image_view(img, media.get(&img.url)),
        Block::Rule => container(space())
            .width(BUBBLE_TEXT_MAX)
            .height(1)
            .style(move |t: &Theme| style::rule(t, mine))
            .into(),
        Block::Blank => space().height(2).into(),
    }
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

async fn load_session() -> Result<Option<Session>, Failure> {
    let config = Config::load()?;
    let Ok(record) = config.find(None) else {
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

async fn finish_login(kind: AccountKind, code: DeviceCode) -> Result<(), Failure> {
    let mut config = Config::load()?;
    let ep = config.endpoints(kind, None);
    let tokens = auth::poll_device_code(&Http::new()?, &ep, &code).await?;
    let store = SecretServiceStore::new().await?;
    let profile = complete_login(kind, &tokens, &store).await?;
    config.upsert(AccountRecord::from_profile(&profile, None));
    config.save()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(estimate_text_width("ok"), 80.0);
        assert_eq!(estimate_text_width(&"x".repeat(500)), BUBBLE_TEXT_MAX);
    }

    #[test]
    fn truncate_counts_chars() {
        assert_eq!(truncate("žluťoučký", 5), "žluť…");
        assert_eq!(truncate("krátký", 10), "krátký");
    }
}
