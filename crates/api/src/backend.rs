//! Implementace [`ChatBackend`] nad interními API Teams.
//!
//! Pracovní i osobní účty sdílí stejný protokol chat služby; liší se jen
//! autoritou, client id a hostiteli, což zachycuje [`Endpoints`].

use std::future::Future;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::Duration;
use tokio::sync::Mutex;
use tymy_core::{
    AccountKind, ChatBackend, Conversation, ConversationId, ConversationPage, Error, Event,
    MessagePage, Person, Profile, Result, SentMessage, text,
};

use crate::auth::{self, CredentialStore, SkypeSession};
use crate::realtime::registrar;
use crate::realtime::trouter::{self, Delivery, TlsTrouter};
use crate::{Endpoints, chat, http::Http, media};

/// Za jak dlouho zkusit registraci trouteru znovu, když se nepovedla.
const REGISTER_RETRY: std::time::Duration = std::time::Duration::from_secs(120);

/// Skypetoken obnovujeme s předstihem, aby nevypršel uprostřed požadavku.
const REFRESH_MARGIN_MINUTES: i64 = 5;

pub struct TeamsBackend {
    profile: Profile,
    endpoints: Endpoints,
    http: Http,
    credentials: Arc<dyn CredentialStore>,
    session: Mutex<Option<SkypeSession>>,
    /// Access token pro Microsoft Graph (plánování schůzek) a jeho platnost.
    graph_token: Mutex<Option<auth::TokenSet>>,
    /// Access tokeny pro další rozsahy (přítomnost, trouter) podle rozsahu.
    scoped_tokens: Mutex<std::collections::HashMap<String, auth::TokenSet>>,
    /// Oddělený zámek: čekání na událost ho drží dlouho a nesmí blokovat ostatní volání.
    trouter: Mutex<Option<TlsTrouter>>,
    /// Adresa a id připojeného trouteru (pro odběry), dokud spojení žije.
    trouter_info: Mutex<Option<trouter::Info>>,
    /// Kdy jsme adresu trouteru naposledy zaregistrovali pro oznámení o zprávách.
    registered_at: Mutex<Option<std::time::Instant>>,
    /// Id tohoto klienta jako koncového bodu (trouter, hlášení stavu); nové při každém spuštění.
    endpoint_id: String,
    /// Uživatelé, jejichž stav odebíráme.
    presence_watch: Mutex<Vec<tymy_core::UserId>>,
}

impl TeamsBackend {
    pub fn new(
        profile: Profile,
        endpoints: Endpoints,
        http: Http,
        credentials: Arc<dyn CredentialStore>,
    ) -> Self {
        Self {
            profile,
            endpoints,
            http,
            credentials,
            session: Mutex::new(None),
            graph_token: Mutex::new(None),
            scoped_tokens: Mutex::new(Default::default()),
            trouter: Mutex::new(None),
            trouter_info: Mutex::new(None),
            endpoint_id: uuid::Uuid::new_v4().to_string(),
            presence_watch: Mutex::new(vec![]),
            registered_at: Mutex::new(None),
        }
    }

    /// Vrátí platnou relaci; v případě potřeby obnoví tokeny.
    pub async fn session(&self) -> Result<SkypeSession> {
        let mut guard = self.session.lock().await;
        if let Some(s) = guard
            .as_ref()
            .filter(|s| s.valid_for(Duration::minutes(REFRESH_MARGIN_MINUTES)))
        {
            return Ok(s.clone());
        }
        let account = &self.profile.account_id;
        let refresh_token = self.credentials.load(account).await?.ok_or_else(|| {
            Error::ReauthRequired(format!("pro účet {account} chybí uložené přihlášení"))
        })?;
        let tokens = auth::refresh(
            &self.http,
            &self.endpoints,
            &refresh_token,
            &self.endpoints.skype_scope,
        )
        .await?;
        if let Some(new_rt) = tokens
            .refresh_token
            .as_deref()
            .filter(|t| *t != refresh_token)
        {
            self.credentials.save(account, new_rt).await?;
        }
        let session =
            auth::exchange_for_skype_token(&self.http, &self.endpoints, &tokens.access_token)
                .await?;
        tracing::debug!(?session, "nová relace");
        *guard = Some(session.clone());
        Ok(session)
    }

    /// Access token pro jiný rozsah (např. Outlook) ze stejného přihlášení.
    pub async fn access_token(&self, scope: &str) -> Result<String> {
        Ok(self.token_set(scope).await?.access_token)
    }

    async fn token_set(&self, scope: &str) -> Result<auth::TokenSet> {
        let account = &self.profile.account_id;
        let refresh_token = self.credentials.load(account).await?.ok_or_else(|| {
            Error::ReauthRequired(format!("pro účet {account} chybí uložené přihlášení"))
        })?;
        let tokens = auth::refresh(&self.http, &self.endpoints, &refresh_token, scope).await?;
        if let Some(new_rt) = tokens
            .refresh_token
            .as_deref()
            .filter(|t| *t != refresh_token)
        {
            self.credentials.save(account, new_rt).await?;
        }
        Ok(tokens)
    }

    /// Pošle serveru aktuální seznam odebíraných stavů, pokud je trouter připojený
    /// (jinak se pošle po připojení).
    async fn subscribe_presence(&self) -> Result<()> {
        let users = self.presence_watch.lock().await.clone();
        let Some(info) = self.trouter_info.lock().await.clone() else {
            return Ok(());
        };
        if users.is_empty() {
            return Ok(());
        }
        let base = self.ups_base().await?;
        let token = self.scoped_token(crate::presence::PRESENCE_SCOPE).await?;
        crate::presence::subscribe(
            &self.http,
            &base,
            &token,
            &info.epid,
            &info.surl,
            &self.endpoints.media_client_version,
            &users,
        )
        .await
    }

    /// Ladění protokolu: obecný požadavek na službu přítomnosti; vrátí stav a tělo odpovědi.
    /// `direct` = přímo na `presence.teams.microsoft.com/v1/` místo přes `…/ups/<region>/v1/`.
    pub async fn ups_debug(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<&serde_json::Value>,
        direct: bool,
    ) -> Result<(u16, String)> {
        let base = if direct {
            url::Url::parse("https://presence.teams.microsoft.com/v1/")
                .map_err(|e| Error::Other(e.to_string()))?
        } else {
            self.ups_base().await?
        };
        let url = base
            .join(path.trim_start_matches('/'))
            .map_err(|e| Error::Other(e.to_string()))?;
        let token = self.scoped_token(crate::presence::PRESENCE_SCOPE).await?;
        let mut rb = self
            .http
            .client
            .request(method, url)
            .bearer_auth(token)
            .header("x-ms-endpoint-id", &self.endpoint_id)
            .header("x-ms-client-user-agent", "Teams-V2-Web")
            .header("x-ms-correlation-id", uuid::Uuid::new_v4().to_string())
            .header("x-ms-client-version", &self.endpoints.media_client_version);
        if let Some(b) = body {
            rb = rb.json(b);
        }
        let resp = rb.send().await.map_err(|e| Error::Network(e.to_string()))?;
        let status = resp.status().as_u16();
        let text = resp.text().await.unwrap_or_default();
        Ok((status, text))
    }

    fn endpoints_kind(&self) -> AccountKind {
        self.profile.kind
    }

    async fn ups_base(&self) -> Result<url::Url> {
        let session = self.session().await?;
        if let Some(ups) = session.services.ups {
            return Ok(ups);
        }
        let mt = session
            .middle_tier
            .as_ref()
            .ok_or_else(|| Error::Protocol("authz odpověď neobsahovala middleTier".into()))?;
        crate::presence::ups_base(mt)
    }

    /// Access token pro `scope` z mezipaměti; obnoví se s předstihem před vypršením.
    pub async fn scoped_token(&self, scope: &str) -> Result<String> {
        let mut guard = self.scoped_tokens.lock().await;
        if let Some(t) = guard.get(scope).filter(|t| {
            t.expires_at - Duration::minutes(REFRESH_MARGIN_MINUTES) > chrono::Utc::now()
        }) {
            return Ok(t.access_token.clone());
        }
        let t = self.token_set(scope).await?;
        let access = t.access_token.clone();
        guard.insert(scope.to_owned(), t);
        Ok(access)
    }

    /// Požadavek na Microsoft Graph (token v mezipaměti, při 401 jednou nový).
    pub async fn graph(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<&serde_json::Value>,
    ) -> Result<serde_json::Value> {
        for attempt in 0..2 {
            let token = {
                let mut guard = self.graph_token.lock().await;
                let fresh = guard.as_ref().filter(|t| {
                    t.expires_at - Duration::minutes(REFRESH_MARGIN_MINUTES) > chrono::Utc::now()
                });
                match fresh {
                    Some(t) => t.access_token.clone(),
                    None => {
                        let t = self.token_set(crate::graph::GRAPH_SCOPE).await?;
                        let access = t.access_token.clone();
                        *guard = Some(t);
                        access
                    }
                }
            };
            match crate::graph::request(&self.http, &token, method.clone(), path, body).await {
                Err(Error::Auth(msg)) if attempt == 0 => {
                    tracing::info!(%msg, "Graph odmítl token, obnovuji");
                    *self.graph_token.lock().await = None;
                }
                r => return r,
            }
        }
        Err(Error::Other(
            "Graph: přihlášení se nepodařilo obnovit".into(),
        ))
    }

    /// Ladění: GET na Graph.
    pub async fn graph_get(&self, path: &str) -> Result<serde_json::Value> {
        self.graph(reqwest::Method::GET, path, None).await
    }

    async fn invalidate_session(&self) {
        *self.session.lock().await = None;
    }

    /// Provede požadavek; při 401 jednou obnoví relaci a zkusí to znovu.
    async fn call<T, F, Fut>(&self, f: F) -> Result<T>
    where
        F: Fn(SkypeSession) -> Fut,
        Fut: Future<Output = Result<T>>,
    {
        match f(self.session().await?).await {
            Err(Error::Auth(msg)) => {
                tracing::info!(%msg, "relace odmítnuta, obnovuji");
                self.invalidate_session().await;
                f(self.session().await?).await
            }
            r => r,
        }
    }

    /// Ladění protokolu: požadavek na middle tier.
    pub async fn mt_request(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<&serde_json::Value>,
    ) -> Result<serde_json::Value> {
        self.call(|s| {
            let method = method.clone();
            async move { crate::people::mt_request(&self.http, &s, method, path, body).await }
        })
        .await
    }

    /// Ladění protokolu: surový GET na chat službu.
    pub async fn raw_get(&self, path_and_query: &str) -> Result<serde_json::Value> {
        self.call(|s| async move { chat::raw_get(&self.http, &s, path_and_query).await })
            .await
    }

    /// Nahraje obrázek do media služby s právem čtení pro chat; vrátí id objektu.
    /// Zprávu neodesílá.
    pub async fn upload_image(
        &self,
        conversation: &ConversationId,
        bytes: &[u8],
        filename: &str,
    ) -> Result<String> {
        self.call(|s| async move {
            media::upload_image(
                &self.http,
                &s,
                conversation,
                bytes,
                filename,
                &self.endpoints.media_client_version,
            )
            .await
        })
        .await
    }

    pub fn http(&self) -> &Http {
        &self.http
    }
}

#[async_trait]
impl ChatBackend for TeamsBackend {
    fn kind(&self) -> AccountKind {
        self.profile.kind
    }

    async fn profile(&self) -> Result<Profile> {
        Ok(self.profile.clone())
    }

    async fn conversations(&self) -> Result<Vec<Conversation>> {
        Ok(self
            .conversation_pages(None, chat::MAX_CONVERSATION_PAGES)
            .await?
            .conversations)
    }

    async fn recent_conversations(&self) -> Result<Vec<Conversation>> {
        Ok(self.conversation_pages(None, 1).await?.conversations)
    }

    async fn conversation_pages(
        &self,
        cursor: Option<&str>,
        pages: usize,
    ) -> Result<ConversationPage> {
        self.call(|s| async move { chat::list_conversations(&self.http, &s, cursor, pages).await })
            .await
    }

    async fn messages(
        &self,
        conversation: &ConversationId,
        cursor: Option<&str>,
    ) -> Result<MessagePage> {
        self.call(
            |s| async move { chat::list_messages(&self.http, &s, conversation, cursor).await },
        )
        .await
    }

    async fn fetch_media(&self, url: &str) -> Result<Vec<u8>> {
        self.call(|s| async move { media::fetch(&self.http, &s, url).await })
            .await
    }

    async fn create_chat(
        &self,
        members: &[tymy_core::UserId],
        topic: Option<&str>,
    ) -> Result<ConversationId> {
        let me = format!("8:orgid:{}", self.profile.account_id);
        let members: Vec<&str> = members.iter().map(|m| m.as_str()).collect();
        self.call(|s| {
            let (me, members) = (&me, &members);
            async move { chat::create_thread(&self.http, &s, me, members, topic).await }
        })
        .await
    }

    async fn profiles(&self, mris: &[tymy_core::UserId]) -> Result<Vec<Person>> {
        let mris: Vec<&str> = mris.iter().map(|m| m.as_str()).collect();
        self.call(|s| {
            let mris = &mris;
            async move { crate::people::fetch_profiles(&self.http, &s, mris).await }
        })
        .await
    }

    async fn members(&self, conversation: &ConversationId) -> Result<Vec<tymy_core::UserId>> {
        self.call(|s| async move { chat::thread_members(&self.http, &s, conversation).await })
            .await
    }

    async fn set_topic(&self, conversation: &ConversationId, topic: &str) -> Result<()> {
        self.call(|s| async move { chat::set_topic(&self.http, &s, conversation, topic).await })
            .await
    }

    async fn add_members(
        &self,
        conversation: &ConversationId,
        members: &[tymy_core::UserId],
    ) -> Result<()> {
        for m in members {
            self.call(|s| async move { chat::add_member(&self.http, &s, conversation, m).await })
                .await?;
        }
        Ok(())
    }

    async fn remove_member(
        &self,
        conversation: &ConversationId,
        member: &tymy_core::UserId,
    ) -> Result<()> {
        self.call(
            |s| async move { chat::remove_member(&self.http, &s, conversation, member).await },
        )
        .await
    }

    async fn leave(&self, conversation: &ConversationId) -> Result<()> {
        let me = tymy_core::UserId(format!("8:orgid:{}", self.profile.account_id));
        self.remove_member(conversation, &me).await
    }

    async fn search_people(&self, query: &str) -> Result<Vec<Person>> {
        self.call(|s| async move { crate::people::search_directory(&self.http, &s, query).await })
            .await
    }

    async fn find_external(&self, email: &str) -> Result<Vec<Person>> {
        self.call(|s| async move { crate::people::search_external(&self.http, &s, email).await })
            .await
    }

    async fn presence(
        &self,
        users: &[tymy_core::UserId],
    ) -> Result<Vec<(tymy_core::UserId, tymy_core::Presence)>> {
        if users.is_empty() {
            return Ok(vec![]);
        }
        let base = self.ups_base().await?;
        let token = self.scoped_token(crate::presence::PRESENCE_SCOPE).await?;
        crate::presence::get_presence(&self.http, &base, &token, users).await
    }

    async fn create_meeting(&self, meeting: &tymy_core::NewMeeting) -> Result<String> {
        let body = crate::graph::event_body(meeting);
        let v = self
            .graph(reqwest::Method::POST, "/v1.0/me/events", Some(&body))
            .await?;
        crate::graph::created_id(&v)
    }

    async fn cancel_meeting(&self, event_id: &str, comment: &str) -> Result<()> {
        let body = serde_json::json!({ "comment": comment });
        self.graph(
            reqwest::Method::POST,
            &crate::graph::event_path(event_id, "/cancel"),
            Some(&body),
        )
        .await
        .map(drop)
    }

    async fn reply_to_event(&self, event_id: &str, reply: tymy_core::EventReply) -> Result<()> {
        let action = match reply {
            tymy_core::EventReply::Accept => "/accept",
            tymy_core::EventReply::Tentative => "/tentativelyAccept",
            tymy_core::EventReply::Decline => "/decline",
        };
        let body = serde_json::json!({ "sendResponse": true });
        self.graph(
            reqwest::Method::POST,
            &crate::graph::event_path(event_id, action),
            Some(&body),
        )
        .await
        .map(drop)
    }

    async fn calendar(
        &self,
        from: chrono::DateTime<chrono::Utc>,
        to: chrono::DateTime<chrono::Utc>,
    ) -> Result<Vec<tymy_core::CalendarEvent>> {
        self.call(|s| async move { crate::calendar::calendar_view(&self.http, &s, from, to).await })
            .await
    }

    async fn send(
        &self,
        conversation: &ConversationId,
        message: &tymy_core::Outgoing,
    ) -> Result<SentMessage> {
        let mut html = message
            .reply_to
            .as_ref()
            .map(text::reply_html)
            .unwrap_or_default();
        let mut mentions = vec![];
        if !message.text.trim().is_empty() {
            let (body, used) = text::compose_html(&message.text, &message.mentions);
            html.push_str(&body);
            mentions = used;
        }
        let mut ams = vec![];
        if !message.images.is_empty() {
            let session = self.session().await?;
            for img in &message.images {
                let id = self
                    .upload_image(conversation, &img.bytes, &img.filename)
                    .await?;
                html.push_str(&media::image_html(&session, &id, img.width, img.height));
                ams.push(id);
            }
        }
        let name = self.profile.display_name.as_deref();
        self.call(|s| {
            let (html, ams, mentions) = (&html, &ams, &mentions);
            async move {
                chat::send_html(&self.http, &s, conversation, html, name, ams, mentions).await
            }
        })
        .await
    }

    async fn history(
        &self,
        conversation: &ConversationId,
        cursor: Option<&str>,
    ) -> Result<MessagePage> {
        self.call(|s| async move {
            chat::list_messages_sized(
                &self.http,
                &s,
                conversation,
                cursor,
                chat::HISTORY_PAGE_SIZE,
            )
            .await
        })
        .await
    }

    async fn mark_read(
        &self,
        conversation: &ConversationId,
        message: &tymy_core::MessageId,
        client_id: Option<&str>,
    ) -> Result<()> {
        let horizon =
            chat::consumption_horizon(message, client_id, chrono::Utc::now().timestamp_millis());
        self.call(|s| {
            let horizon = horizon.clone();
            async move {
                chat::set_consumption_horizon(&self.http, &s, conversation, &horizon).await
            }
        })
        .await
    }

    async fn react(
        &self,
        conversation: &ConversationId,
        message: &tymy_core::MessageId,
        key: &str,
        on: bool,
    ) -> Result<()> {
        self.call(|s| async move {
            chat::set_reaction(&self.http, &s, conversation, message, key, on).await
        })
        .await
    }

    async fn edit(
        &self,
        conversation: &ConversationId,
        message: &tymy_core::MessageId,
        html: &str,
    ) -> Result<()> {
        let name = self.profile.display_name.as_deref();
        self.call(|s| async move {
            chat::edit_html(&self.http, &s, conversation, message, html, name).await
        })
        .await
    }

    async fn delete(
        &self,
        conversation: &ConversationId,
        message: &tymy_core::MessageId,
    ) -> Result<()> {
        self.call(
            |s| async move { chat::delete_message(&self.http, &s, conversation, message).await },
        )
        .await
    }

    async fn poll_events(&self) -> Result<Vec<Event>> {
        let mut guard = self.trouter.lock().await;
        if guard.is_none() {
            let token = self.scoped_token(trouter::TROUTER_SCOPE).await?;
            // adresa pro region účtu z authz; ruční přepis v endpoints.toml má přednost
            let services = self.session().await?.services;
            let default = Endpoints::defaults(self.endpoints_kind()).trouter_url;
            let url = services
                .trouter
                .filter(|_| self.endpoints.trouter_url == default)
                .unwrap_or_else(|| self.endpoints.trouter_url.clone());
            let t = TlsTrouter::connect(
                &url,
                &token,
                &self.endpoints.media_client_version,
                &self.endpoint_id,
            )
            .await?;
            *self.trouter_info.lock().await = Some(t.info.clone());
            *guard = Some(t);
            *self.registered_at.lock().await = None;
            if let Err(e) = self.subscribe_presence().await {
                tracing::warn!(error = %e, "odběr stavů po připojení trouteru");
            }
        }
        let Some(t) = guard.as_mut() else {
            return Ok(vec![]);
        };
        // registrace pro oznámení o zprávách: po připojení a pak s předstihem před vypršením
        let renew = std::time::Duration::from_secs(registrar::TTL_SECS * 3 / 4);
        let age = self.registered_at.lock().await.map(|at| at.elapsed());
        if age.is_none_or(|a| a >= renew) {
            let session = self.session().await?;
            let default = Endpoints::defaults(self.endpoints_kind()).registrar_url;
            let registrar_url = session
                .services
                .registrar
                .clone()
                .filter(|_| self.endpoints.registrar_url == default)
                .unwrap_or_else(|| self.endpoints.registrar_url.clone());
            match registrar::register(
                &self.http,
                &registrar_url,
                &session.token,
                &t.info.epid,
                &t.info.surl,
                &self.endpoints.media_client_version,
            )
            .await
            {
                Ok(()) => {
                    tracing::info!("trouter zaregistrován pro oznámení o zprávách");
                    *self.registered_at.lock().await = Some(std::time::Instant::now());
                    return Ok(vec![Event::Connected { messaging: true }]);
                }
                // bez registrace dál chodí aspoň stavy; zkusí se znovu za chvíli
                Err(e) => {
                    tracing::warn!(error = %e, "registrace trouteru");
                    *self.registered_at.lock().await =
                        Some(std::time::Instant::now() - renew + REGISTER_RETRY);
                    return Ok(vec![Event::Connected { messaging: false }]);
                }
            }
        }
        let wait = renew
            .saturating_sub(age.unwrap_or_default())
            .max(std::time::Duration::from_secs(1));
        match tokio::time::timeout(wait, t.next()).await {
            // čas obnovit registraci
            Err(_) => Ok(vec![]),
            Ok(Ok(d)) => Ok(events_from(d)),
            Ok(Err(e)) => {
                // příští volání se připojí znovu
                *guard = None;
                *self.trouter_info.lock().await = None;
                Err(e)
            }
        }
    }

    async fn publish_presence(&self, availability: tymy_core::Availability) -> Result<()> {
        let base = self.ups_base().await?;
        let token = self.scoped_token(crate::presence::PRESENCE_SCOPE).await?;
        crate::presence::publish(
            &self.http,
            &base,
            &token,
            &self.endpoint_id,
            &self.endpoints.media_client_version,
            availability,
        )
        .await
    }

    async fn typing(&self, conversation: &ConversationId) -> Result<()> {
        self.call(|s| async move { chat::send_typing(&self.http, &s, conversation).await })
            .await
    }

    async fn set_status(&self, status: Option<tymy_core::Availability>) -> Result<()> {
        let base = self.ups_base().await?;
        let token = self.scoped_token(crate::presence::PRESENCE_SCOPE).await?;
        crate::presence::force(
            &self.http,
            &base,
            &token,
            &self.endpoint_id,
            &self.endpoints.media_client_version,
            status,
        )
        .await
    }

    async fn watch_presence(&self, users: &[tymy_core::UserId]) -> Result<()> {
        *self.presence_watch.lock().await = users.to_vec();
        self.subscribe_presence().await
    }
}

/// Dokončí přihlášení: uloží refresh token a vrátí profil účtu.
pub async fn complete_login(
    kind: AccountKind,
    tokens: &auth::TokenSet,
    credentials: &dyn CredentialStore,
) -> Result<Profile> {
    let profile = auth::profile_from_tokens(kind, tokens)?;
    let rt = tokens.refresh_token.as_deref().ok_or_else(|| {
        Error::Auth("server nevrátil refresh token (chybí offline_access?)".into())
    })?;
    credentials.save(&profile.account_id, rt).await?;
    Ok(profile)
}

/// Převede požadavek doručený trouterem na události.
fn events_from(d: Delivery) -> Vec<Event> {
    match d.path.trim_end_matches('/') {
        "unifiedPresenceService" => {
            let list = crate::presence::parse_notification(&d.body);
            if list.is_empty() {
                vec![]
            } else {
                vec![Event::Presence(list)]
            }
        }
        "messaging" => serde_json::from_str::<chat::wire::EventMessage>(&d.body)
            .map(|e| vec![e.into_domain()])
            .unwrap_or_default(),
        other => vec![Event::Unknown {
            resource_type: other.to_owned(),
            raw: serde_json::Value::Null,
        }],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::MemoryStore;
    use serde_json::json;
    use tymy_core::AccountId;
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    async fn setup() -> (MockServer, TeamsBackend, Arc<MemoryStore>) {
        let server = MockServer::start().await;
        let ep = Endpoints {
            authority: format!("{}/organizations", server.uri()),
            authz_url: format!("{}/authz", server.uri()),
            chat_service: server.uri(),
            ..Default::default()
        };
        let store = Arc::new(MemoryStore::default());
        let profile = Profile {
            account_id: AccountId("acc".into()),
            kind: AccountKind::Work,
            display_name: Some("Jan".into()),
            username: None,
            tenant_id: None,
        };
        store.save(&profile.account_id, "rt-old").await.unwrap();
        let backend = TeamsBackend::new(profile, ep, Http::new().unwrap(), store.clone());
        (server, backend, store)
    }

    async fn mount_auth(server: &MockServer) {
        Mock::given(method("POST"))
            .and(path("/organizations/oauth2/v2.0/token"))
            .and(body_string_contains("grant_type=refresh_token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "at", "refresh_token": "rt-new", "expires_in": 3600
            })))
            .mount(server)
            .await;
        Mock::given(method("POST"))
            .and(path("/authz"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "tokens": {"skypeToken": "st", "expiresIn": 3600},
                "regionGtms": {"chatService": server.uri()}
            })))
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn refresh_rotates_token_and_caches_session() {
        let (server, backend, store) = setup().await;
        mount_auth(&server).await;
        let s1 = backend.session().await.unwrap();
        let s2 = backend.session().await.unwrap();
        assert_eq!(s1.token, "st");
        assert_eq!(s1.expires_at, s2.expires_at, "druhé volání použije cache");
        assert_eq!(
            store
                .load(&AccountId("acc".into()))
                .await
                .unwrap()
                .as_deref(),
            Some("rt-new")
        );
        // token endpoint se volal jen jednou
        let refreshes = server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|r| r.url.path().ends_with("/token"))
            .count();
        assert_eq!(refreshes, 1);
    }

    #[tokio::test]
    async fn retries_once_after_401() {
        let (server, backend, _store) = setup().await;
        mount_auth(&server).await;
        Mock::given(method("GET"))
            .and(path("/v1/users/ME/conversations"))
            .respond_with(ResponseTemplate::new(401))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/users/ME/conversations"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "conversations": [{"id": "19:x@thread.v2", "threadProperties": {"topic": "T", "threadType": "chat"}}]
            })))
            .mount(&server)
            .await;
        let convs = backend.conversations().await.unwrap();
        assert_eq!(convs.len(), 1);
        assert_eq!(convs[0].topic.as_deref(), Some("T"));
    }

    #[test]
    fn converts_trouter_deliveries() {
        let msg = Delivery {
            path: "messaging".into(),
            body: serde_json::json!({
                "type": "EventMessage",
                "resourceType": "NewMessage",
                "resourceLink": "https://n.example/v1/users/ME/conversations/19:abc@thread.v2/messages/5",
                "resource": {"id": "5", "messagetype": "RichText/Html", "content": "<p>Ahoj</p>",
                    "from": "https://n.example/v1/users/ME/contacts/8:orgid:aaaa",
                    "composetime": "2026-11-10T09:00:00.000Z"},
            })
            .to_string(),
        };
        match events_from(msg).as_slice() {
            [Event::NewMessage(m)] => {
                assert_eq!(m.conversation_id.as_str(), "19:abc@thread.v2");
                assert_eq!(m.content, "<p>Ahoj</p>");
            }
            e => panic!("{e:?}"),
        }
        let stop = Delivery {
            path: "messaging".into(),
            body: serde_json::json!({
                "resourceType": "NewMessage",
                "resourceLink": "https://n.example/v1/users/ME/conversations/19:abc@thread.v2/messages/6",
                "resource": {"id": "6", "messagetype": "Control/ClearTyping",
                    "from": "https://n.example/v1/users/ME/contacts/8:orgid:aaaa"},
            })
            .to_string(),
        };
        assert!(matches!(
            events_from(stop).as_slice(),
            [Event::Typing { active: false, .. }]
        ));
        let presence = Delivery {
            path: "unifiedPresenceService".into(),
            body: r#"{"presence":[{"mri":"8:orgid:aaaa","presence":{"availability":"Busy"}}]}"#
                .into(),
        };
        assert!(matches!(events_from(presence).as_slice(), [Event::Presence(l)] if l.len() == 1));
        let call = Delivery {
            path: "callAgent/u/r/call/end/".into(),
            body: "{}".into(),
        };
        assert!(matches!(
            events_from(call).as_slice(),
            [Event::Unknown { .. }]
        ));
        let junk = Delivery {
            path: "messaging".into(),
            body: "nesmysl".into(),
        };
        assert!(events_from(junk).is_empty());
    }

    #[tokio::test]
    async fn missing_credentials_require_reauth() {
        let (_server, backend, store) = setup().await;
        store.delete(&AccountId("acc".into())).await.unwrap();
        assert!(matches!(
            backend.session().await.unwrap_err(),
            Error::ReauthRequired(_)
        ));
    }
}
