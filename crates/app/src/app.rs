//! Hlavní okno: seznam chatů vlevo, vlákno zpráv vpravo (rozložení jako Skype).

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Local, Utc};
use iced::widget::{
    Column, Id, button, column, container, image, mouse_area, operation, row, rule, scrollable,
    space, stack, text, text_input,
};
use iced::{Border, Color, Element, Font, Length, Subscription, Task, Theme, font};
use tymy_api::auth::{self, DeviceCode, SecretServiceStore};
use tymy_api::{Http, TeamsBackend, complete_login};
use tymy_config::{AccountRecord, Config};
use tymy_core::text::{InlineImage, Part, message_parts};
use tymy_core::{
    AccountId, AccountKind, ChatBackend, Conversation, ConversationId, ConversationKind, Error,
    Message, MessageId, MessageKind, MessagePage, Person, Profile, one_on_one_id,
};

use tymy_store::Store;

use crate::state::{
    PeerNames, channel_name, group_teams, is_me, merge_messages, move_pin, order_chats,
};

const COMPOSER: Id = Id::new("composer");
const MESSAGES: Id = Id::new("messages");
/// Dokud nemáme trouter (WebSocket), obnovujeme seznam chatů periodicky.
const REFRESH: Duration = Duration::from_secs(5);
/// Kolik chatů bez známého jména projít při startu kvůli jménům účastníků.
const PEER_SCAN_LIMIT: usize = 25;
const BOLD: Font = Font {
    weight: font::Weight::Bold,
    ..Font::DEFAULT
};

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
    /// Myš nad položkou seznamu (zobrazí ovládání připnutí).
    Hover(ConversationId),
    Unhover(ConversationId),
    Pin(ConversationId),
    Unpin(ConversationId),
    /// Posun připnutého chatu (záporně = nahoru).
    MovePin(ConversationId, isize),
    Draft(String),
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
            Msg::Scrolled(_) | Msg::Draft(_) | Msg::Search(_) => {
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

/// Hledání lidí: pauza v psaní před dotazem a minimální délka dotazu.
const SEARCH_DEBOUNCE: Duration = Duration::from_millis(300);
const MIN_PEOPLE_QUERY: usize = 2;

/// Za „dole“ ve vláknu považujeme i pozici do tolika pixelů od konce.
const BOTTOM_TOLERANCE: f32 = 40.0;

/// Horní mez velikosti obrázků v cache.
const MEDIA_CACHE_LIMIT: u64 = 500 * 1024 * 1024;

/// Kolik zpráv konverzace načíst z cache při otevření.
const CACHED_MESSAGES: usize = 300;

/// Maximální šířka obrázku v bublině.
const IMAGE_MAX_WIDTH: f32 = 360.0;

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
    draft: String,
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
            draft: String::new(),
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
        (app, Task::perform(load_session(), Msg::Booted))
    }

    pub fn title(&self) -> String {
        let unread = self.conversations.iter().filter(|c| self.unread(c)).count();
        if unread > 0 {
            format!("({unread}) Tymy")
        } else {
            "Tymy".into()
        }
    }

    pub fn subscription(&self) -> Subscription<Msg> {
        match self.phase {
            Phase::Ready(_) => iced::time::every(REFRESH).map(|_| Msg::Tick),
            _ => Subscription::none(),
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
        let media = self.fetch_media_for(&msgs);
        self.messages.insert(id.clone(), msgs);
        media
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
                let mut tasks = vec![];
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
                self.selected = Some(id.clone());
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
                if self.tab != Tab::Chats || self.search.trim().chars().count() < MIN_PEOPLE_QUERY {
                    self.people.clear();
                    self.searching_people = false;
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
            Msg::Draft(s) => {
                self.draft = s;
                Task::none()
            }
            Msg::Send => {
                let text = self.draft.trim().to_owned();
                let (Some(id), Some(backend)) = (
                    self.selected.clone(),
                    self.session().map(|s| s.backend.clone()),
                ) else {
                    return Task::none();
                };
                if text.is_empty() || self.sending {
                    return Task::none();
                }
                self.sending = true;
                self.draft.clear();
                // U nového chatu: když vlákno neexistuje, založit ho a poslat znovu.
                let draft_peer = self.draft_people.get(&id).map(|p| p.mri.clone());
                Task::perform(
                    {
                        let (id, text) = (id.clone(), text.clone());
                        async move {
                            match backend.send_text(&id, &text).await {
                                Ok(_) => Ok(id),
                                Err(Error::Http { status: 404, .. }) if draft_peer.is_some() => {
                                    let peer = draft_peer.expect("ověřeno výše");
                                    tracing::info!(%id, "vlákno neexistuje, zakládám chat");
                                    let created = backend.create_chat(&[peer]).await?;
                                    backend.send_text(&created, &text).await?;
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
                // vrátit text, ať se neztratí
                if self.draft.is_empty() {
                    self.draft = text;
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
            Phase::Starting => centered(text("Načítám…").size(16)),
            Phase::Login { code, busy } => self.login_view(code.as_ref(), *busy),
            Phase::Ready(session) => row![
                self.sidebar(session),
                rule::vertical(1),
                self.chat_pane(session),
            ]
            .into(),
        };
        match &self.error {
            Some(err) => column![error_banner(err), body].into(),
            None => body,
        }
    }

    fn login_view(&self, code: Option<&DeviceCode>, busy: bool) -> Element<'_, Msg> {
        let content: Element<'_, Msg> = match code {
            Some(code) => column![
                text("Přihlášení").size(24).font(BOLD),
                text("1. Otevřete přihlašovací stránku Microsoftu:"),
                button(text(code.verification_uri.clone()))
                    .style(button::text)
                    .on_press(Msg::OpenUrl(code.verification_uri.clone())),
                text("2. Zadejte tento kód:"),
                text(code.user_code.clone()).size(32).font(BOLD),
                text("3. Potvrďte přihlášení. Toto okno pak pokračuje samo.").style(muted),
            ]
            .spacing(12)
            .into(),
            None => column![
                text("Tymy").size(32).font(BOLD),
                text("Lehký klient Teams pro Linux").style(muted),
                space().height(16),
                button(text("Přihlásit pracovní / školní účet"))
                    .padding([10, 20])
                    .on_press_maybe((!busy).then_some(Msg::StartLogin(AccountKind::Work))),
                button(text("Přihlásit osobní účet (experimentální)"))
                    .padding([10, 20])
                    .style(button::secondary)
                    .on_press_maybe((!busy).then_some(Msg::StartLogin(AccountKind::Personal))),
            ]
            .spacing(12)
            .into(),
        };
        centered(content)
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
        let tabs = row![
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
        .spacing(4);
        let (items, placeholder) = match self.tab {
            Tab::Chats => (self.chat_list(session, &query), "Hledat chat nebo osobu…"),
            Tab::Teams => (self.team_list(&query), "Hledat tým nebo kanál…"),
        };
        column![
            container(text(me).size(16).font(BOLD)).padding([12, 14]),
            container(tabs).padding([0, 8]),
            space().height(6),
            container(
                text_input(placeholder, &self.search)
                    .on_input(Msg::Search)
                    .padding(8)
            )
            .padding([0, 8]),
            space().height(6),
            scrollable(Column::with_children(items).spacing(2).padding([0, 6]))
                .height(Length::Fill),
        ]
        .width(300)
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
            items.push(section_label("Připnuté"));
            let n = pinned.len();
            for (i, c) in pinned.into_iter().enumerate() {
                items.push(self.chat_row(session, c, Some((i, n))));
            }
            if !rest.is_empty() {
                items.push(section_label("Ostatní"));
            }
        }
        items.extend(rest.into_iter().map(|c| self.chat_row(session, c, None)));

        if query.chars().count() >= MIN_PEOPLE_QUERY {
            items.push(section_label("Lidé"));
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
        items
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
            let mut header = row![text(g.name.clone()).size(13).font(BOLD)]
                .spacing(6)
                .align_y(iced::Alignment::Center);
            if unread > 0 {
                header = header.push(space::horizontal()).push(badge(unread));
            }
            items.push(
                container(header)
                    .width(Length::Fill)
                    .padding(iced::Padding {
                        top: 10.0,
                        right: 10.0,
                        bottom: 2.0,
                        left: 10.0,
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
        items
    }

    fn channel_item<'a>(&'a self, c: &'a Conversation) -> Element<'a, Msg> {
        let selected = self.selected.as_ref() == Some(&c.id);
        let unread = self.unread(c);
        let font = if unread { BOLD } else { Font::DEFAULT };
        button(
            row![
                text("#").size(13).style(muted),
                text(truncate(&channel_name(c), 30)).size(13).font(font),
                space::horizontal(),
                text(short_time(c.last_activity)).size(11).style(muted),
                text(if unread { "●" } else { "" }).size(12).style(accent),
            ]
            .spacing(6),
        )
        .width(Length::Fill)
        .padding(iced::Padding {
            top: 6.0,
            right: 10.0,
            bottom: 6.0,
            left: 20.0,
        })
        .style(move |t: &Theme, status| chat_item_style(t, status, selected))
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
            Some((i, n)) if hovered => row![
                small_button(
                    "▲",
                    (i > 0).then(|| Msg::MovePin(c.id.clone(), -1)),
                    "Posunout nahoru"
                ),
                small_button(
                    "▼",
                    (i + 1 < n).then(|| Msg::MovePin(c.id.clone(), 1)),
                    "Posunout dolů"
                ),
                small_button("✕", Some(Msg::Unpin(c.id.clone())), "Odepnout"),
            ]
            .into(),
            Some(_) => container(text("📌").size(11).style(muted))
                .center_x(Length::Fill)
                .into(),
            None if hovered => small_button("📌", Some(Msg::Pin(c.id.clone())), "Připnout nahoru"),
            None => space().into(),
        };
        let width = if pin.is_some() { 72.0 } else { 28.0 };
        mouse_area(
            row![
                self.chat_item(session, c),
                container(controls).width(width).center_y(Length::Shrink),
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
            .unwrap_or_default();
        let title_font = if unread { BOLD } else { Font::DEFAULT };
        let top = row![
            text(truncate(&self.peers.title(c), 24))
                .size(14)
                .font(title_font),
            space::horizontal(),
            text(short_time(c.last_activity)).size(11).style(muted),
        ]
        .spacing(6);
        let bottom = row![
            text(truncate(&preview, 32)).size(12).style(muted),
            space::horizontal(),
            text(if unread { "●" } else { "" }).size(12).style(accent),
        ];
        button(column![top, bottom].spacing(2))
            .width(Length::Fill)
            .padding([8, 10])
            .style(move |t: &Theme, status| chat_item_style(t, status, selected))
            .on_press(Msg::Select(c.id.clone()))
            .into()
    }

    fn chat_pane<'a>(&'a self, session: &'a Session) -> Element<'a, Msg> {
        let Some(conv) = self
            .selected
            .as_ref()
            .and_then(|id| self.conversations.iter().find(|c| &c.id == id))
        else {
            return centered(text("Vyberte chat vlevo").style(muted));
        };
        let header = container(text(self.conv_title(conv, true)).size(18).font(BOLD))
            .padding([12, 16])
            .width(Length::Fill);

        let mut list = Column::new().spacing(4).padding([12, 16]);
        match self.older.get(&conv.id) {
            Some(Some(_)) => {
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
                        .style(button::text)
                        .on_press_maybe((!self.loading_older).then_some(Msg::LoadOlder)),
                    )
                    .center_x(Length::Fill),
                );
            }
            None => {
                list = list.push(container(text("Načítám…").style(muted)).center_x(Length::Fill))
            }
            Some(None) => {}
        }
        if self.older.get(&conv.id) == Some(&None)
            && self
                .messages
                .get(&conv.id)
                .is_none_or(|m| m.iter().all(|m| !displayable(m)))
        {
            list = list.push(
                container(text("Zatím žádné zprávy. Napište první zprávu.").style(muted))
                    .center_x(Length::Fill)
                    .padding([24, 0]),
            );
        }
        let msgs = self
            .messages
            .get(&conv.id)
            .map(Vec::as_slice)
            .unwrap_or_default();
        let mut prev: Option<&Message> = None;
        for m in msgs.iter().filter(|m| displayable(m)) {
            let day = m.arrived_at.map(|t| t.with_timezone(&Local).date_naive());
            let prev_day = prev
                .and_then(|p| p.arrived_at)
                .map(|t| t.with_timezone(&Local).date_naive());
            if day.is_some() && day != prev_day {
                list = list.push(
                    container(
                        text(
                            day.map(|d| d.format("%-d. %-m. %Y").to_string())
                                .unwrap_or_default(),
                        )
                        .size(11)
                        .style(muted),
                    )
                    .center_x(Length::Fill)
                    .padding([8, 0]),
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
            ));
            prev = Some(m);
        }

        let composer = row![
            text_input("Napište zprávu…", &self.draft)
                .id(COMPOSER)
                .on_input(Msg::Draft)
                .on_submit(Msg::Send)
                .padding(10),
            button(text("Odeslat")).padding([10, 16]).on_press_maybe(
                (!self.sending && !self.draft.trim().is_empty()).then_some(Msg::Send)
            ),
        ]
        .spacing(8)
        .padding(12);

        column![
            header,
            rule::horizontal(1),
            self.thread_view(list),
            rule::horizontal(1),
            composer,
        ]
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
            .height(Length::Fill);
        if !self.new_below {
            return thread.into();
        }
        let pill = button(text("↓ Nové zprávy").size(13))
            .padding([6, 14])
            .style(|t: &Theme, status| {
                let mut style = button::primary(t, status);
                style.border.radius = 14.0.into();
                style
            })
            .on_press(Msg::JumpToNewest);
        stack![
            thread,
            container(pill)
                .align_bottom(Length::Fill)
                .center_x(Length::Fill)
                .padding(12),
        ]
        .into()
    }
}

fn tab_button<'a>(label: &str, unread: usize, active: bool, tab: Tab) -> Element<'a, Msg> {
    let font = if active { BOLD } else { Font::DEFAULT };
    let mut content = row![text(label.to_owned()).size(13).font(font)]
        .spacing(6)
        .align_y(iced::Alignment::Center);
    if unread > 0 {
        content = content.push(badge(unread));
    }
    button(content)
        .padding([6, 12])
        .style(move |t: &Theme, status| chat_item_style(t, status, active))
        .on_press(Msg::SetTab(tab))
        .into()
}

/// Odznak s počtem nepřečtených (barva zvýraznění, „99+“ nad 99).
fn badge<'a>(count: usize) -> Element<'a, Msg> {
    let label = if count > 99 {
        "99+".to_owned()
    } else {
        count.to_string()
    };
    container(text(label).size(10).font(BOLD))
        .padding([1, 6])
        .style(|t: &Theme| {
            let p = t.extended_palette();
            container::Style {
                background: Some(p.primary.base.color.into()),
                text_color: Some(p.primary.base.text),
                border: Border {
                    radius: 8.0.into(),
                    ..Default::default()
                },
                ..Default::default()
            }
        })
        .into()
}

fn section_label<'a>(label: &'a str) -> Element<'a, Msg> {
    container(text(label).size(11).font(BOLD).style(muted))
        .padding(iced::Padding {
            top: 8.0,
            right: 10.0,
            bottom: 2.0,
            left: 10.0,
        })
        .into()
}

fn note<'a>(t: &str) -> Element<'a, Msg> {
    container(text(t.to_owned()).size(11).style(muted))
        .padding([4, 10])
        .into()
}

fn small_button<'a>(label: &'a str, msg: Option<Msg>, tip: &'a str) -> Element<'a, Msg> {
    iced::widget::tooltip(
        button(text(label).size(11))
            .padding([2, 4])
            .style(button::text)
            .on_press_maybe(msg),
        container(text(tip).size(11))
            .padding([2, 6])
            .style(container::rounded_box),
        iced::widget::tooltip::Position::Bottom,
    )
    .into()
}

fn looks_like_email(q: &str) -> bool {
    let q = q.trim();
    q.split_once('@')
        .is_some_and(|(user, domain)| !user.is_empty() && domain.contains('.') && !q.contains(' '))
}

fn person_item(p: &Person) -> Element<'_, Msg> {
    let mut title = row![text(p.display_name.as_str()).size(14)].spacing(6);
    if p.external {
        title = title.push(text("externí").size(11).style(accent));
    }
    let subtitle = p.subtitle();
    button(column![title, text(truncate(&subtitle, 40)).size(12).style(muted)].spacing(2))
        .width(Length::Fill)
        .padding([8, 10])
        .style(|t: &Theme, status| chat_item_style(t, status, false))
        .on_press(Msg::OpenPerson(p.clone()))
        .into()
}

fn displayable(m: &Message) -> bool {
    m.deleted
        || m.is_user_content()
        || matches!(&m.kind, MessageKind::Event(t) if t.starts_with("Event/Call"))
}

fn message_view<'a>(
    m: &'a Message,
    mine: bool,
    header: bool,
    media: &'a HashMap<String, Media>,
    me: &str,
) -> Element<'a, Msg> {
    let mut body = Column::new().spacing(4);
    if m.deleted {
        body = body.push(text("Zpráva byla smazána").size(13).style(muted));
    } else if let MessageKind::Event(_) = m.kind {
        body = body.push(text("📞 Hovor").size(13).style(muted));
    } else {
        for part in message_parts(&m.content) {
            body = body.push(match part {
                Part::Text(t) => Element::from(text(t).size(14)),
                Part::Image(img) => image_view(&img, media.get(&img.url)),
            });
        }
        for a in &m.attachments {
            let label = text(format!("📎 {}", a.name)).size(13);
            body = body.push(match &a.url {
                Some(url) => Element::from(
                    button(label)
                        .style(button::text)
                        .padding(0)
                        .on_press(Msg::OpenUrl(url.clone())),
                ),
                None => label.into(),
            });
        }
        if m.edited_at.is_some() {
            body = body.push(text("upraveno").size(10).style(muted));
        }
    }
    let bubble = container(body)
        .padding([6, 10])
        .max_width(560)
        .style(move |t: &Theme| bubble_style(t, mine));

    let mut col = Column::new().spacing(2);
    if header {
        let time = short_time(m.arrived_at.or(m.composed_at));
        let label = if mine {
            time
        } else {
            format!("{} · {time}", m.sender_name.as_deref().unwrap_or("?"))
        };
        let label = text(label).size(11).style(muted);
        col = col.push(space().height(6));
        col = col.push(if mine {
            Element::from(row![space::horizontal(), label])
        } else {
            label.into()
        });
    }
    col = col.push(if mine {
        Element::from(row![space::horizontal(), bubble])
    } else {
        Element::from(row![bubble, space::horizontal()])
    });
    if !m.deleted && !m.reactions.is_empty() {
        let chips = row(m.reactions.iter().map(|r| {
            let by_me = r.users.iter().any(|u| u.as_str().ends_with(me));
            let label = if r.users.len() > 1 {
                format!("{} {}", r.emoji(), r.users.len())
            } else {
                r.emoji()
            };
            container(text(label).size(12))
                .padding([1, 7])
                .style(move |t: &Theme| reaction_style(t, by_me))
                .into()
        }))
        .spacing(4);
        col = col.push(if mine {
            Element::from(row![space::horizontal(), chips])
        } else {
            Element::from(row![space().width(6), chips])
        });
    }
    col.into()
}

fn reaction_style(t: &Theme, by_me: bool) -> container::Style {
    let p = t.extended_palette();
    container::Style {
        background: Some(p.background.weak.color.into()),
        text_color: Some(p.background.base.text),
        border: Border {
            radius: 10.0.into(),
            width: 1.0,
            color: if by_me {
                p.primary.base.color
            } else {
                p.background.strong.color
            },
        },
        ..Default::default()
    }
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
        Some(Media::Ready(handle, _)) => button(image(handle.clone()).width(w).height(h))
            .style(button::text)
            .padding(0)
            .on_press(Msg::OpenImage(img.clone()))
            .into(),
        Some(Media::Failed) => text("🖼 Obrázek se nepodařilo načíst")
            .size(13)
            .style(muted)
            .into(),
        _ => container(text("Načítám obrázek…").size(12).style(muted))
            .width(w)
            .height(h)
            .center_x(w)
            .center_y(h)
            .into(),
    }
}

/// Text pro náhled v seznamu chatů; obrázky jako „🖼 Obrázek“.
fn preview_text(html: &str) -> String {
    let parts: Vec<String> = message_parts(html)
        .into_iter()
        .map(|p| match p {
            Part::Text(t) => t.replace('\n', " "),
            Part::Image(_) => "🖼 Obrázek".into(),
        })
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
            text(err).size(13),
            space::horizontal(),
            button(text("×"))
                .style(button::text)
                .on_press(Msg::DismissError),
        ]
        .spacing(8),
    )
    .padding([6, 12])
    .width(Length::Fill)
    .style(|t: &Theme| {
        let p = t.extended_palette();
        container::Style {
            background: Some(p.danger.weak.color.into()),
            text_color: Some(p.danger.weak.text),
            ..Default::default()
        }
    })
    .into()
}

fn muted(t: &Theme) -> text::Style {
    let c = t.extended_palette().background.base.text;
    text::Style {
        color: Some(Color { a: 0.6, ..c }),
    }
}

fn accent(t: &Theme) -> text::Style {
    text::Style {
        color: Some(t.extended_palette().primary.base.color),
    }
}

fn chat_item_style(t: &Theme, status: button::Status, selected: bool) -> button::Style {
    let p = t.extended_palette();
    let background = if selected {
        Some(p.primary.weak.color.into())
    } else if matches!(status, button::Status::Hovered) {
        Some(p.background.weak.color.into())
    } else {
        None
    };
    button::Style {
        background,
        text_color: if selected {
            p.primary.weak.text
        } else {
            p.background.base.text
        },
        border: Border {
            radius: 6.0.into(),
            ..Default::default()
        },
        ..Default::default()
    }
}

fn bubble_style(t: &Theme, mine: bool) -> container::Style {
    let p = t.extended_palette();
    let pair = if mine {
        p.primary.weak
    } else {
        p.background.weak
    };
    container::Style {
        background: Some(pair.color.into()),
        text_color: Some(pair.text),
        border: Border {
            radius: 10.0.into(),
            ..Default::default()
        },
        ..Default::default()
    }
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

fn open_url(url: &str) {
    if !url.starts_with("https://") {
        tracing::warn!(%url, "odmítám otevřít jiný než https odkaz");
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
    fn truncate_counts_chars() {
        assert_eq!(truncate("žluťoučký", 5), "žluť…");
        assert_eq!(truncate("krátký", 10), "krátký");
    }
}
