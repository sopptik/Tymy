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
use crate::realtime::poll::{self, PollRegistration};
use crate::{Endpoints, chat, http::Http, media};

/// Skypetoken obnovujeme s předstihem, aby nevypršel uprostřed požadavku.
const REFRESH_MARGIN_MINUTES: i64 = 5;

pub struct TeamsBackend {
    profile: Profile,
    endpoints: Endpoints,
    http: Http,
    credentials: Arc<dyn CredentialStore>,
    session: Mutex<Option<SkypeSession>>,
    /// Oddělený zámek: long-poll ho drží desítky sekund a nesmí blokovat ostatní volání.
    poll: Mutex<Option<PollRegistration>>,
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
            poll: Mutex::new(None),
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

    async fn search_people(&self, query: &str) -> Result<Vec<Person>> {
        self.call(|s| async move { crate::people::search_directory(&self.http, &s, query).await })
            .await
    }

    async fn find_external(&self, email: &str) -> Result<Vec<Person>> {
        self.call(|s| async move { crate::people::search_external(&self.http, &s, email).await })
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
        if !message.text.trim().is_empty() {
            html.push_str(&text::plain_to_html(&message.text));
        }
        if !message.images.is_empty() {
            let session = self.session().await?;
            for img in &message.images {
                let id = self
                    .call(|s| async move {
                        media::upload_image(&self.http, &s, conversation, &img.bytes, &img.filename)
                            .await
                    })
                    .await?;
                html.push_str(&media::image_html(&session, &id, img.width, img.height));
            }
        }
        let name = self.profile.display_name.as_deref();
        self.call(|s| {
            let html = &html;
            async move { chat::send_html(&self.http, &s, conversation, html, name).await }
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
        let session = self.session().await?;
        let mut reg = self.poll.lock().await;
        if reg.is_none() {
            *reg = Some(poll::register(&self.http, &session).await?);
        }
        let current = reg.clone().unwrap_or_default();
        match poll::poll(&self.http, &session, &current).await {
            Ok(events) => Ok(events),
            Err(e) => {
                // Registrace mohla vypršet; příště ji vytvoříme znovu.
                *reg = None;
                match e {
                    Error::Auth(_) => {
                        self.invalidate_session().await;
                        Err(e)
                    }
                    // Zaniklá subscription: nic nehlásíme, další volání se přeregistruje.
                    Error::Http {
                        status: 404 | 410, ..
                    } => Ok(vec![]),
                    e => Err(e),
                }
            }
        }
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
