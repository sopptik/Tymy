//! Hlavní okno: seznam chatů vlevo, vlákno zpráv vpravo (rozložení jako Skype).

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Local, Utc};
use iced::widget::{
    Column, Id, button, column, container, image, operation, row, rule, scrollable, space, text,
    text_input,
};
use iced::{Border, Color, Element, Font, Length, Subscription, Task, Theme, font};
use tymy_api::auth::{self, DeviceCode, SecretServiceStore};
use tymy_api::{Http, TeamsBackend, complete_login};
use tymy_config::{AccountRecord, Config};
use tymy_core::text::{InlineImage, Part, message_parts};
use tymy_core::{
    AccountKind, ChatBackend, Conversation, ConversationId, Error, Message, MessageId, MessageKind,
    MessagePage, Profile,
};

use crate::state::{PeerNames, is_me, merge_messages};

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
}

impl From<Error> for Failure {
    fn from(e: Error) -> Self {
        Self {
            reauth: matches!(e, Error::ReauthRequired(_)),
            text: e.to_string(),
        }
    }
}

impl From<anyhow::Error> for Failure {
    fn from(e: anyhow::Error) -> Self {
        Self {
            text: format!("{e:#}"),
            reauth: false,
        }
    }
}

#[derive(Clone)]
pub struct Session {
    profile: Profile,
    backend: Arc<TeamsBackend>,
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
    Search(String),
    Draft(String),
    Send,
    Sent(ConversationId, String, Result<(), Failure>),
    DismissError,
    MediaLoaded(String, Result<Arc<Vec<u8>>, Failure>),
    OpenImage(InlineImage),
    /// Stažený originál obrázku: (URL originálu, URL náhledu, výsledek).
    FullImage(String, String, Result<Arc<Vec<u8>>, Failure>),
}

/// Stav staženého obrázku ze zprávy.
enum Media {
    Loading,
    Ready(image::Handle, Arc<Vec<u8>>),
    Failed,
}

/// Maximální šířka obrázku v bublině.
const IMAGE_MAX_WIDTH: f32 = 360.0;

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
    peer_scan_done: bool,
    error: Option<String>,
    /// Obrázky ze zpráv podle URL.
    media: HashMap<String, Media>,
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
            peer_scan_done: false,
            error: None,
            media: HashMap::new(),
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
        tracing::warn!(error = %f.text, "chyba");
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
            async move {
                backend
                    .messages(&id, cursor.as_deref())
                    .await
                    .map_err(Failure::from)
                    .map(|p| (id, p))
            },
            move |r| match r {
                Ok((id, page)) => Msg::Messages(id, Ok(page), older),
                Err(e) => Msg::Messages(ConversationId(String::new()), Err(e), older),
            },
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
        match msg {
            Msg::Booted(Ok(Some(session))) => {
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
                self.conversations = list.into_iter().filter(Conversation::is_chat).collect();
                let Some(session) = self.session().cloned() else {
                    return Task::none();
                };
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
                        .filter(|c| c.topic.is_none() && !self.peers.knows(&c.id))
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
                }
                Task::none()
            }

            Msg::Select(id) => {
                self.selected = Some(id.clone());
                self.mark_read(&id);
                let fetch = if self.messages.contains_key(&id) {
                    Task::none()
                } else {
                    self.fetch_messages(id, None)
                };
                Task::batch([
                    fetch,
                    operation::focus(COMPOSER),
                    operation::snap_to_end(MESSAGES),
                ])
            }
            Msg::Messages(id, Ok(page), older) => {
                if older {
                    self.loading_older = false;
                }
                if let Some(s) = self.session().cloned() {
                    self.peers.learn(&s.profile, page.messages.iter().rev());
                }
                let first_load = !self.older.contains_key(&id);
                if older || first_load {
                    self.older.insert(id.clone(), page.older);
                }
                let media = self.fetch_media_for(&page.messages);
                merge_messages(self.messages.entry(id.clone()).or_default(), page.messages);
                if self.selected.as_ref() == Some(&id) {
                    self.mark_read(&id);
                }
                media
            }
            Msg::Messages(_, Err(e), older) => {
                if older {
                    self.loading_older = false;
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
                Task::perform(
                    {
                        let (id, text) = (id.clone(), text.clone());
                        async move {
                            backend
                                .send_text(&id, &text)
                                .await
                                .map(|_| ())
                                .map_err(Failure::from)
                        }
                    },
                    move |r| Msg::Sent(id.clone(), text.clone(), r),
                )
            }
            Msg::Sent(id, _, Ok(())) => {
                self.sending = false;
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
            Msg::DismissError => {
                self.error = None;
                Task::none()
            }
            Msg::MediaLoaded(url, Ok(bytes)) => {
                let handle = image::Handle::from_bytes(bytes.as_ref().clone());
                self.media.insert(url, Media::Ready(handle, bytes));
                Task::none()
            }
            Msg::MediaLoaded(url, Err(e)) => {
                tracing::warn!(%url, error = %e.text, "obrázek se nepodařilo stáhnout");
                self.media.insert(url, Media::Failed);
                Task::none()
            }
            Msg::OpenImage(img) => {
                let full = img.full_size_url();
                if let Some(path) = self.originals.get(&full) {
                    open_path(path);
                    return Task::none();
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
                match save_media(&full, &bytes) {
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
                    match save_media(&preview, bytes) {
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
        let query = self.search.to_lowercase();
        let items = self
            .conversations
            .iter()
            .filter(|c| query.is_empty() || self.peers.title(c).to_lowercase().contains(&query))
            .map(|c| self.chat_item(session, c));
        column![
            container(text(me).size(16).font(BOLD)).padding([12, 14]),
            container(
                text_input("Hledat chat…", &self.search)
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
            text(truncate(&self.peers.title(c), 28))
                .size(14)
                .font(title_font),
            space::horizontal(),
            text(short_time(c.last_activity)).size(11).style(muted),
        ]
        .spacing(6);
        let bottom = row![
            text(truncate(&preview, 38)).size(12).style(muted),
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
        let header = container(text(self.peers.title(conv)).size(18).font(BOLD))
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
            scrollable(list)
                .id(MESSAGES)
                .anchor_bottom()
                .height(Length::Fill),
            rule::horizontal(1),
            composer,
        ]
        .width(Length::Fill)
        .into()
    }
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
    col.into()
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

/// Uloží médium do `~/.cache/tymy/media/` (název podle URL, přípona podle obsahu).
fn save_media(url: &str, bytes: &[u8]) -> std::io::Result<std::path::PathBuf> {
    use std::hash::{Hash, Hasher};
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
    let dirs = directories::ProjectDirs::from("", "", "tymy")
        .ok_or_else(|| std::io::Error::other("nelze určit adresář cache"))?;
    let dir = dirs.cache_dir().join("media");
    let mut h = std::collections::hash_map::DefaultHasher::new();
    url.hash(&mut h);
    let path = dir.join(format!("{:016x}.{ext}", h.finish()));
    std::fs::create_dir_all(&dir)?;
    std::fs::write(&path, bytes)?;
    Ok(path)
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
    let backend = TeamsBackend::new(profile.clone(), ep, Http::new()?, Arc::new(store));
    Ok(Some(Session {
        profile,
        backend: Arc::new(backend),
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
