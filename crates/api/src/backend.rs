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
use crate::calls;
use crate::realtime::registrar;
use crate::realtime::trouter::{self, Delivery, TlsTrouter};
use crate::{Endpoints, chat, http::Http, media};

/// Za jak dlouho zkusit registraci trouteru znovu, když se nepovedla.
const REGISTER_RETRY: std::time::Duration = std::time::Duration::from_secs(120);

/// Za jak dlouho se znovu připojit spojením pro hovory, když spadlo.
const CALLS_RETRY: std::time::Duration = std::time::Duration::from_secs(30);
/// Zvonící hovor, o jehož konci nepřišla zpráva, po této době zapomeneme.
const RINGING_MAX: std::time::Duration = std::time::Duration::from_secs(120);

/// Co hlásí úloha, která drží spojení trouteru pro hovory.
enum CallsFeed {
    Connected(trouter::Info),
    Delivery(Delivery),
    Failed(Error),
}

/// Spojení trouteru pro hovory (`SkypeSpaces`). Běží ve vlastní úloze, aby čekání na zprávy
/// chatu nepřerušovalo jeho čtení (a naopak); doručení předává kanálem.
struct CallsLink {
    rx: tokio::sync::mpsc::Receiver<CallsFeed>,
    task: tokio::task::JoinHandle<()>,
    info: Option<trouter::Info>,
    registered_at: Option<std::time::Instant>,
}

impl Drop for CallsLink {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[derive(Default)]
struct CallsState {
    link: Option<CallsLink>,
    /// Dřív se znovu nepřipojovat (po výpadku).
    retry_at: Option<std::time::Instant>,
}

/// Příchozí hovor, který u nás právě zvoní.
struct Ringing {
    call_id: String,
    /// `<uuid>` adres pro události tohoto hovoru (viz [`calls::Callbacks::id`]).
    agent: String,
    reject: url::Url,
    since: std::time::Instant,
    /// Přijetí (`callInvitation.links.acceptance` z odpovědi `attach`).
    acceptance: Option<url::Url>,
    /// Nabídka SDP volajícího (dialekt Microsoftu) a `mediaLegId`.
    offer: Option<(String, String)>,
    participant_id: String,
    group: bool,
}

/// Jak zavěsit probíhající hovor.
enum Hangup {
    /// Odchozí: odejít z konverzace (`links.leave`).
    Leave(url::Url),
    /// Přijatý: `DELETE` na `callLeg`.
    CallLeg(url::Url),
}

/// Probíhající hovor (odchozí nebo přijatý).
struct ActiveCall {
    call_id: String,
    agent: String,
    media: Arc<dyn tymy_core::Media>,
    me: calls::Caller,
    media_leg_id: String,
    /// Odpověď SDP protistrany už máme (u odchozího).
    answered: bool,
    hangup: Hangup,
    /// Skupinový hovor: odchodem ostatních nekončí.
    group: bool,
    /// V hovoru byl někdo další (počet účastníků aspoň 2).
    joined: bool,
}

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
    /// Druhé spojení trouteru, na které chodí oznámení o příchozích hovorech.
    calls: Mutex<CallsState>,
    /// Id koncového bodu spojení pro hovory (web klient má pro každé spojení vlastní).
    calls_epid: String,
    ringing: Mutex<Vec<Ringing>>,
    active: Mutex<Vec<ActiveCall>>,
    /// Adresa spojení pro hovory (kopie mimo zámek `calls`, který drží čekání na události).
    calls_info: Mutex<Option<trouter::Info>>,
    /// Přihlašovací údaje k relay serverům a do kdy je používat.
    turn: Mutex<Option<(calls::TurnCredentials, std::time::Instant)>>,
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
            calls: Mutex::new(CallsState::default()),
            calls_epid: uuid::Uuid::new_v4().to_string(),
            ringing: Mutex::new(vec![]),
            active: Mutex::new(vec![]),
            calls_info: Mutex::new(None),
            turn: Mutex::new(None),
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

    /// Zkouška signalizace hovoru: testovací hovor s echo botem bez médií. Otevře vlastní
    /// spojení trouteru, založí hovor, `listen` čeká na události (přijetí serverem
    /// s odpovědí SDP, účastníci) a zavěsí. Vrací popis jednotlivých kroků.
    pub async fn call_probe(
        &self,
        listen: std::time::Duration,
    ) -> Result<crate::calls::ProbeReport> {
        use crate::calls::{self, Callbacks, Caller, ProbeReport};
        let mut log = vec![];
        let mut accepted = false;
        let session = self.session().await?;
        let ic3 = self.scoped_token(trouter::TROUTER_SCOPE).await?;
        let trouter_url = session
            .services
            .trouter
            .clone()
            .unwrap_or_else(|| self.endpoints.trouter_url.clone());
        let epid = uuid::Uuid::new_v4().to_string();
        let mut t = TlsTrouter::connect(
            &trouter_url,
            &ic3,
            &self.endpoints.media_client_version,
            &epid,
            trouter::Client::Chat,
        )
        .await?;
        log.push("trouter pro hovor připojen".to_owned());
        let cb = Callbacks::new(&t.info.surl);
        let ack = calls::acceptance_ack(&cb).to_string();
        t.set_responder(Box::new(move |d| {
            (calls::event_name(&d.path).as_deref() == Some("call/acceptance")).then(|| ack.clone())
        }));
        let me = tymy_core::UserId(format!("8:orgid:{}", self.profile.account_id));
        let caller = Caller {
            mri: me,
            display_name: self.profile.display_name.clone().unwrap_or_default(),
            endpoint_id: epid,
            participant_id: uuid::Uuid::new_v4().to_string(),
        };
        let client = format!(
            "SkypeSpaces/{}/os=linux; osVer=undefined; deviceType=computer; browser=chrome",
            self.endpoints.media_client_version
        );
        let chain = uuid::Uuid::new_v4().to_string();
        // autorizaci služby hovorů záznam neobsahoval: zkusit známé kombinace
        let auths: [(&str, Option<&str>, bool); 3] = [
            ("skypetoken", None, true),
            (
                "Bearer (spaces) + skypetoken",
                Some(session.bearer.as_str()),
                true,
            ),
            ("Bearer (ic3)", Some(ic3.as_str()), false),
        ];
        let send = |url: url::Url,
                    body: serde_json::Value,
                    auth: (&str, Option<&str>, bool),
                    put: bool| {
            let mut rb = if put {
                self.http.client.put(url)
            } else {
                self.http.client.post(url)
            };
            rb = rb
                .header("x-microsoft-skype-chain-id", &chain)
                .header(
                    "x-microsoft-skype-message-id",
                    uuid::Uuid::new_v4().to_string(),
                )
                .header("x-microsoft-skype-client", &client)
                .header("x-ms-migration", "True")
                .json(&body);
            if let Some(b) = auth.1 {
                rb = rb.bearer_auth(b);
            }
            if auth.2 {
                rb = rb.header("X-Skypetoken", &session.token);
            }
            async move {
                let resp = rb.send().await.map_err(|e| Error::Network(e.to_string()))?;
                let status = resp.status().as_u16();
                let text = resp.text().await.unwrap_or_default();
                Ok::<_, Error>((status, text))
            }
        };
        let service = session
            .services
            .call_service
            .clone()
            .unwrap_or_else(|| calls::DEFAULT_CALL_SERVICE.to_owned());
        let service = url::Url::parse(&service).map_err(|e| Error::Other(e.to_string()))?;
        let sdp = calls::probe_offer();
        let body = calls::conversation_body(
            &caller,
            &cb,
            &sdp,
            &client,
            Some("UserInitiatedTestCall"),
            false,
        );
        let mut created = None;
        for auth in auths {
            let (status, text) = send(service.clone(), body.clone(), auth, false).await?;
            log.push(format!("založení hovoru [{}]: HTTP {status}", auth.0));
            if (200..300).contains(&status) {
                created = Some((auth, text));
                break;
            }
            log.push(format!(
                "  odpověď: {}",
                text.chars().take(300).collect::<String>()
            ));
        }
        let Some((auth, text)) = created else {
            return Ok(ProbeReport {
                steps: log,
                accepted,
            });
        };
        let conv: serde_json::Value =
            serde_json::from_str(&text).map_err(|e| Error::Protocol(e.to_string()))?;
        if let Some(kind) = conv
            .pointer("/state/conversationType")
            .and_then(|v| v.as_str())
        {
            log.push(format!("  konverzace typu {kind}"));
        }
        let leave = calls::link(&conv, "/links/leave")?;
        // od této chvíle hovor existuje: ať se stane cokoli, na konci zavěsit
        let rest = async {
            let add = calls::link(&conv, "/links/addParticipant")?;
            let bot = calls::ECHO_BOT.trim_start_matches("28:");
            let thread = format!("19:{}_{bot}@unq.gbl.spaces", self.profile.account_id);
            let body = calls::add_body(&caller, &cb, calls::ECHO_BOT, &thread);
            let (status, text) = send(add, body, auth, false).await?;
            log.push(format!("pozvání echo bota: HTTP {status}"));
            if !(200..300).contains(&status) {
                log.push(format!(
                    "  odpověď: {}",
                    text.chars().take(300).collect::<String>()
                ));
            }
            let until = tokio::time::Instant::now() + listen;
            while let Ok(next) = tokio::time::timeout_at(until, t.next()).await {
                let d = next?;
                let name = calls::event_name(&d.path).unwrap_or_else(|| d.path.clone());
                let v: serde_json::Value = serde_json::from_str(&d.body).unwrap_or_default();
                let detail = match name.as_str() {
                    "call/acceptance" => v
                        .pointer("/callAcceptance/mediaContent/blob")
                        .and_then(|b| b.as_str())
                        .map(|sdp| {
                            accepted = true;
                            format!("odpověď SDP: {}", calls::describe_sdp(sdp))
                        })
                        .unwrap_or_else(|| "bez SDP".into()),
                    "call/end" | "conversation/conversationEnd" => format!(
                        "{} {}",
                        v.pointer("/callEnd/phrase")
                            .or_else(|| v.pointer("/conversationEnd/phrase"))
                            .and_then(|p| p.as_str())
                            .unwrap_or("?"),
                        v.pointer("/callEnd/code")
                            .or_else(|| v.pointer("/conversationEnd/code"))
                            .map(|c| c.to_string())
                            .unwrap_or_default()
                    ),
                    "conversation/rosterUpdate" => {
                        let n = v
                            .get("participants")
                            .and_then(|p| p.as_object())
                            .map(|p| p.len())
                            .unwrap_or(0);
                        format!("účastníků ve změně: {n}")
                    }
                    "conversation/addParticipantSuccess" => "protistrana připojena".into(),
                    "conversation/addParticipantFailure" | "call/redirection" => {
                        d.body.chars().take(300).collect()
                    }
                    _ => String::new(),
                };
                log.push(format!("událost {name} {detail}").trim_end().to_owned());
            }
            Ok::<_, Error>(())
        }
        .await;
        let (status, _) = send(leave, calls::leave_body(&caller), auth, false).await?;
        log.push(format!("zavěšení: HTTP {status}"));
        rest?;
        Ok(ProbeReport {
            steps: log,
            accepted,
        })
    }

    /// Adresa trouteru pro region účtu z authz; ruční přepis v endpoints.toml má přednost.
    fn trouter_url(&self, services: &crate::auth::RegionServices) -> String {
        let default = Endpoints::defaults(self.endpoints_kind()).trouter_url;
        services
            .trouter
            .clone()
            .filter(|_| self.endpoints.trouter_url == default)
            .unwrap_or_else(|| self.endpoints.trouter_url.clone())
    }

    fn registrar_url(&self, services: &crate::auth::RegionServices) -> String {
        let default = Endpoints::defaults(self.endpoints_kind()).registrar_url;
        services
            .registrar
            .clone()
            .filter(|_| self.endpoints.registrar_url == default)
            .unwrap_or_else(|| self.endpoints.registrar_url.clone())
    }

    /// Klient v hlavičce `x-microsoft-skype-client` požadavků signalizace.
    fn call_client(&self) -> String {
        format!(
            "SkypeSpaces/{}/os=linux; osVer=undefined; deviceType=computer; browser=chrome",
            self.endpoints.media_client_version
        )
    }

    /// Požadavek signalizace hovoru `call_id` (autorizace skypetokenem jako u zkoušky).
    async fn call_signal(
        &self,
        method: reqwest::Method,
        url: url::Url,
        body: &serde_json::Value,
        call_id: &str,
    ) -> Result<serde_json::Value> {
        let session = self.session().await?;
        let resp = self
            .http
            .client
            .request(method, url)
            .header("X-Skypetoken", &session.token)
            .header("x-microsoft-skype-chain-id", call_id)
            .header(
                "x-microsoft-skype-message-id",
                uuid::Uuid::new_v4().to_string(),
            )
            .header("x-microsoft-skype-client", self.call_client())
            .header("x-ms-migration", "True")
            .json(body)
            .send()
            .await
            .map_err(|e| Error::Network(e.to_string()))?;
        let status = resp.status().as_u16();
        let text = resp.text().await.unwrap_or_default();
        if !(200..300).contains(&status) {
            return Err(Error::Other(format!(
                "signalizace hovoru: HTTP {status} {}",
                text.chars().take(200).collect::<String>()
            )));
        }
        Ok(serde_json::from_str(&text).unwrap_or_default())
    }

    /// Přihlašovací údaje k relay serverům médií (`trap/tokens`), uložené na dobu platnosti.
    /// Odhlásí nás z hovoru (odchozí: `leave`, přijatý: `DELETE callLeg`).
    async fn send_hangup(&self, c: ActiveCall) -> Result<()> {
        let call_id = c.call_id.as_str();
        match c.hangup {
            Hangup::Leave(url) => {
                self.call_signal(
                    reqwest::Method::POST,
                    url,
                    &calls::leave_body(&c.me),
                    call_id,
                )
                .await
            }
            Hangup::CallLeg(url) => {
                self.call_signal(reqwest::Method::DELETE, url, &calls::hangup_body(), call_id)
                    .await
            }
        }
        .map(|_| ())
    }

    /// Spojení pro hovory je připojené (lze volat a přijímat).
    pub async fn calls_ready(&self) -> bool {
        self.calls_info.lock().await.is_some()
    }

    pub async fn turn_credentials(&self) -> Result<calls::TurnCredentials> {
        if let Some((c, until)) = self.turn.lock().await.clone()
            && std::time::Instant::now() < until
        {
            return Ok(c);
        }
        let session = self.session().await?;
        let resp = self
            .http
            .client
            .get(calls::TRAP_TOKENS)
            .header("X-Skypetoken", &session.token)
            .header("api-version", "2")
            .header("x-ms-migration", "True")
            .send()
            .await
            .map_err(|e| Error::Network(e.to_string()))?;
        let status = resp.status().as_u16();
        let v: serde_json::Value = resp.json().await.unwrap_or_default();
        if !(200..300).contains(&status) {
            return Err(Error::Other(format!("trap/tokens: HTTP {status}")));
        }
        let c = calls::parse_trap_tokens(&v)
            .ok_or_else(|| Error::Protocol("trap/tokens bez údajů".into()))?;
        let secs = v.get("expires").and_then(|e| e.as_u64()).unwrap_or(3600);
        // s rezervou, ať údaje nevyprší uprostřed hovoru
        let until = std::time::Instant::now() + std::time::Duration::from_secs(secs / 2);
        *self.turn.lock().await = Some((c.clone(), until));
        Ok(c)
    }

    /// Udrží spojení pro hovory: připojí ho (ve vlastní úloze), zaregistruje a obnovuje
    /// registraci. Chyby jen zaloguje: chat i stavy běží dál i bez hovorů.
    async fn keep_calls_link(&self, state: &mut CallsState) {
        let now = std::time::Instant::now();
        if state.link.is_none() && state.retry_at.is_none_or(|t| now >= t) {
            match self.spawn_calls_link().await {
                Ok(link) => state.link = Some(link),
                Err(e) => {
                    tracing::warn!(error = %e, "spojení pro hovory");
                    state.retry_at = Some(now + CALLS_RETRY);
                }
            }
        }
        let Some(link) = state.link.as_mut() else {
            return;
        };
        let Some(info) = link.info.clone() else {
            return;
        };
        let renew = std::time::Duration::from_secs(registrar::TTL_SECS * 3 / 4);
        if link.registered_at.is_some_and(|at| at.elapsed() < renew) {
            return;
        }
        let result = async {
            let session = self.session().await?;
            registrar::register(
                &self.http,
                registrar::Template::Calls,
                &self.registrar_url(&session.services),
                &session.token,
                &info.epid,
                &info.surl,
                &self.endpoints.media_client_version,
            )
            .await
        }
        .await;
        match result {
            Ok(()) => {
                tracing::info!("trouter zaregistrován pro příchozí hovory");
                link.registered_at = Some(std::time::Instant::now());
            }
            Err(e) => {
                tracing::warn!(error = %e, "registrace pro příchozí hovory");
                link.registered_at = Some(std::time::Instant::now() - renew + REGISTER_RETRY);
            }
        }
    }

    async fn spawn_calls_link(&self) -> Result<CallsLink> {
        let token = self.scoped_token(trouter::TROUTER_SCOPE).await?;
        let url = self.trouter_url(&self.session().await?.services);
        let version = self.endpoints.media_client_version.clone();
        let epid = self.calls_epid.clone();
        let (tx, rx) = tokio::sync::mpsc::channel(16);
        let task = tokio::spawn(async move {
            let mut t =
                match TlsTrouter::connect(&url, &token, &version, &epid, trouter::Client::Calls)
                    .await
                {
                    Ok(t) => t,
                    Err(e) => {
                        let _ = tx.send(CallsFeed::Failed(e)).await;
                        return;
                    }
                };
            if tx.send(CallsFeed::Connected(t.info.clone())).await.is_err() {
                return;
            }
            // přijetí odchozího hovoru server potvrzuje tělem odpovědi s dalšími adresami
            let surl = t.info.surl.clone();
            t.set_responder(Box::new(move |d| {
                if calls::event_name(&d.path).as_deref() != Some("call/acceptance") {
                    return None;
                }
                let agent = calls::agent_id(&d.path)?;
                let cb = calls::Callbacks::with_id(&surl, agent);
                Some(calls::acceptance_ack(&cb).to_string())
            }));
            loop {
                let feed = match t.next().await {
                    Ok(d) => CallsFeed::Delivery(d),
                    Err(e) => CallsFeed::Failed(e),
                };
                let failed = matches!(feed, CallsFeed::Failed(_));
                if tx.send(feed).await.is_err() || failed {
                    return;
                }
            }
        });
        Ok(CallsLink {
            rx,
            task,
            info: None,
            registered_at: None,
        })
    }

    /// Zpracuje zprávu ze spojení pro hovory.
    async fn on_calls_feed(&self, state: &mut CallsState, feed: Option<CallsFeed>) -> Vec<Event> {
        match feed {
            Some(CallsFeed::Connected(info)) => {
                tracing::info!(epid = %info.epid, "trouter pro hovory připojen");
                *self.calls_info.lock().await = Some(info.clone());
                if let Some(link) = state.link.as_mut() {
                    link.info = Some(info);
                    link.registered_at = None;
                }
                vec![]
            }
            Some(CallsFeed::Delivery(d)) => {
                let surl = state
                    .link
                    .as_ref()
                    .and_then(|l| l.info.as_ref())
                    .map(|i| i.surl.clone());
                self.call_events(d, surl.as_deref()).await
            }
            Some(CallsFeed::Failed(e)) => {
                tracing::warn!(error = %e, "spojení pro hovory spadlo");
                *self.calls_info.lock().await = None;
                state.link = None;
                state.retry_at = Some(std::time::Instant::now() + CALLS_RETRY);
                vec![]
            }
            None => {
                *self.calls_info.lock().await = None;
                state.link = None;
                state.retry_at = Some(std::time::Instant::now() + CALLS_RETRY);
                vec![]
            }
        }
    }

    /// Doručení na spojení pro hovory: oznámení o příchozím hovoru (na kořen adresy) nebo
    /// událost hovoru (`callAgent/<uuid>/…`).
    async fn call_events(&self, d: Delivery, surl: Option<&str>) -> Vec<Event> {
        let path = d.path.trim_end_matches('/');
        tracing::debug!(
            event = calls::event_name(path).as_deref().unwrap_or(path),
            "událost hovoru"
        );
        tracing::trace!(body = %d.body, "tělo události hovoru");
        if path.is_empty() {
            return self.on_invite(&d.body, surl).await;
        }
        let (Some(agent), Some(name)) = (calls::agent_id(path), calls::event_name(path)) else {
            return vec![];
        };
        let ended = matches!(name.as_str(), "call/end" | "conversation/conversationEnd");
        {
            let mut ringing = self.ringing.lock().await;
            if let Some(i) = ringing.iter().position(|r| r.agent == agent) {
                if !ended {
                    return vec![];
                }
                let r = ringing.remove(i);
                return vec![Event::CallEnded {
                    call_id: r.call_id,
                    elsewhere: calls::ended_elsewhere(&d.body),
                }];
            }
        }
        let mut active = self.active.lock().await;
        let Some(i) = active.iter().position(|c| c.agent == agent) else {
            return vec![];
        };
        if ended {
            let c = active.remove(i);
            return vec![Event::CallState {
                call_id: c.call_id,
                state: tymy_core::CallState::Ended {
                    reason: calls::end_reason(&d.body),
                },
            }];
        }
        let call = &mut active[i];
        let v: serde_json::Value = serde_json::from_str(&d.body).unwrap_or_default();
        if name == "conversation/rosterUpdate"
            && let Some(n) = calls::participant_count(&v)
        {
            call.joined |= n >= 2;
            // 1:1 (i echo bot): protistrana odešla a my bychom zůstali na schůzce sami
            if n <= 1 && call.joined && !call.group {
                let c = active.remove(i);
                drop(active);
                let call_id = c.call_id.clone();
                if let Err(e) = self.send_hangup(c).await {
                    tracing::warn!(error = %e, "zavěšení po odchodu protistrany");
                }
                return vec![Event::CallState {
                    call_id,
                    state: tymy_core::CallState::Ended {
                        reason: "protistrana odešla".into(),
                    },
                }];
            }
            return vec![];
        }
        match name.as_str() {
            "call/progress" => vec![Event::CallState {
                call_id: call.call_id.clone(),
                state: tymy_core::CallState::Ringing,
            }],
            "call/acceptance" | "call/mediaAnswer" => {
                let pointer = if name == "call/acceptance" {
                    "/callAcceptance/mediaContent"
                } else {
                    "/mediaAnswer/mediaContent"
                };
                if !call.answered
                    && let Some((sdp, _)) = calls::offer_of(&v, pointer)
                {
                    let answer = crate::sdp::from_ms(&sdp);
                    tracing::trace!(sdp = %sdp, "odpověď SDP serveru");
                    match call.media.accept_answer(&answer).await {
                        Ok(()) => call.answered = true,
                        Err(e) => tracing::warn!(error = %e, "odpověď SDP protistrany"),
                    }
                }
                if name == "call/acceptance" {
                    vec![Event::CallState {
                        call_id: call.call_id.clone(),
                        state: tymy_core::CallState::Connected,
                    }]
                } else {
                    vec![]
                }
            }
            "call/mediaRenegotiation" => {
                if let Err(e) = self.renegotiate(call, &v, surl).await {
                    tracing::warn!(error = %e, "přeVyjednání médií");
                }
                vec![]
            }
            _ => vec![],
        }
    }

    /// Server nabízí nová média (např. protistrana zapnula kameru): odpovědět.
    async fn renegotiate(
        &self,
        call: &mut ActiveCall,
        v: &serde_json::Value,
        surl: Option<&str>,
    ) -> Result<()> {
        let (offer, leg) = calls::offer_of(v, "/mediaNegotiation/mediaContent")
            .ok_or_else(|| Error::Protocol("přeVyjednání bez SDP".into()))?;
        let link = calls::link(v, "/mediaNegotiation/links/mediaAnswer")?;
        let answer = call.media.answer(&crate::sdp::from_ms(&offer)).await?;
        let surl = surl.ok_or_else(|| Error::Other("spojení pro hovory nemá adresu".into()))?;
        let cb = calls::Callbacks::with_id(surl, &call.agent);
        call.media_leg_id = leg;
        let body = calls::media_answer_body(
            &call.me,
            &cb,
            &crate::sdp::to_ms(&answer),
            &call.media_leg_id,
            call.media.sends_video(),
        );
        self.call_signal(reqwest::Method::POST, link, &body, &call.call_id)
            .await
            .map(|_| ())
    }

    /// My jako účastník hovoru (`participant_id` přidělený serverem nebo nový).
    fn me_in_call(&self, participant_id: &str) -> calls::Caller {
        calls::Caller {
            mri: tymy_core::UserId(format!("8:orgid:{}", self.profile.account_id)),
            display_name: self.profile.display_name.clone().unwrap_or_default(),
            endpoint_id: self.calls_epid.clone(),
            participant_id: participant_id.to_owned(),
        }
    }

    async fn on_invite(&self, body: &str, surl: Option<&str>) -> Vec<Event> {
        let Some(invite) = calls::parse_invite(body) else {
            tracing::debug!("spojení pro hovory: neznámé oznámení");
            return vec![];
        };
        let mut ringing = self.ringing.lock().await;
        ringing.retain(|r| r.since.elapsed() < RINGING_MAX);
        if ringing.iter().any(|r| r.call_id == invite.call_id) {
            return vec![];
        }
        let event = Event::IncomingCall(tymy_core::IncomingCall {
            call_id: invite.call_id.clone(),
            from: invite.from.clone(),
            from_name: invite.from_name.clone(),
            group: invite.group,
        });
        // ohlásit se a rozezvonit u volajícího; když to nevyjde, aspoň zobrazit
        let rung = match surl {
            Some(surl) => self.ring(&invite, surl).await,
            None => Err(Error::Other("spojení pro hovory nemá adresu".into())),
        };
        let (agent, reject, acceptance) = match rung {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(error = %e, "ohlášení příchozího hovoru");
                (String::new(), invite.reject.clone(), None)
            }
        };
        ringing.push(Ringing {
            call_id: invite.call_id,
            agent,
            reject,
            since: std::time::Instant::now(),
            acceptance,
            offer: invite.offer,
            participant_id: invite.participant_id,
            group: invite.group,
        });
        vec![event]
    }

    /// `attach` + `progress: ringing`. Vrací `<uuid>` adres hovoru, odkaz pro odmítnutí
    /// (a zavěšení) a odkaz pro přijetí.
    async fn ring(
        &self,
        invite: &calls::Invite,
        surl: &str,
    ) -> Result<(String, url::Url, Option<url::Url>)> {
        let cb = calls::Callbacks::new(surl);
        let resp = self
            .call_signal(
                reqwest::Method::POST,
                invite.attach.clone(),
                &calls::attach_body(&cb),
                &invite.call_id,
            )
            .await?;
        let reject = calls::link(&resp, "/callInvitation/links/callLeg")
            .unwrap_or_else(|_| invite.reject.clone());
        let acceptance = calls::link(&resp, "/callInvitation/links/acceptance").ok();
        let me = self.me_in_call(&invite.participant_id);
        if let Ok(progress) = calls::link(&resp, "/callInvitation/links/progress")
            && let Err(e) = self
                .call_signal(
                    reqwest::Method::POST,
                    progress,
                    &calls::progress_body(&me),
                    &invite.call_id,
                )
                .await
        {
            tracing::warn!(error = %e, "hlášení zvonění");
        }
        Ok((cb.id().to_owned(), reject, acceptance))
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
            let url = self.trouter_url(&self.session().await?.services);
            let t = TlsTrouter::connect(
                &url,
                &token,
                &self.endpoints.media_client_version,
                &self.endpoint_id,
                trouter::Client::Chat,
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
            match registrar::register(
                &self.http,
                registrar::Template::Chat,
                &self.registrar_url(&session.services),
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
        let mut calls = self.calls.lock().await;
        self.keep_calls_link(&mut calls).await;
        // probudit se i kvůli obnově spojení nebo registrace pro hovory
        let calls_due = match (&calls.link, calls.retry_at) {
            (Some(l), _) => l
                .registered_at
                .map(|at| renew.saturating_sub(at.elapsed()))
                .unwrap_or(REGISTER_RETRY),
            (None, Some(at)) => at.saturating_duration_since(std::time::Instant::now()),
            (None, None) => CALLS_RETRY,
        };
        let wait = renew
            .saturating_sub(age.unwrap_or_default())
            .min(calls_due)
            .max(std::time::Duration::from_secs(1));
        enum Got {
            Chat(std::result::Result<Result<Delivery>, tokio::time::error::Elapsed>),
            Calls(Option<CallsFeed>),
        }
        let got = {
            let calls_rx = calls.link.as_mut().map(|l| &mut l.rx);
            let calls_next = async {
                match calls_rx {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                r = tokio::time::timeout(wait, t.next()) => Got::Chat(r),
                f = calls_next => Got::Calls(f),
            }
        };
        match got {
            Got::Calls(feed) => Ok(self.on_calls_feed(&mut calls, feed).await),
            // čas obnovit registraci
            Got::Chat(Err(_)) => Ok(vec![]),
            Got::Chat(Ok(Ok(d))) => Ok(events_from(d)),
            Got::Chat(Ok(Err(e))) => {
                // příští volání se připojí znovu
                *guard = None;
                *self.trouter_info.lock().await = None;
                Err(e)
            }
        }
    }

    async fn start_call(
        &self,
        target: tymy_core::CallTarget,
        media: Arc<dyn tymy_core::Media>,
    ) -> Result<String> {
        let surl = self
            .calls_info
            .lock()
            .await
            .clone()
            .map(|i| i.surl)
            .ok_or_else(|| Error::Other("služba hovorů zatím není připojená".into()))?;
        let session = self.session().await?;
        let call_id = uuid::Uuid::new_v4().to_string();
        let cb = calls::Callbacks::new(&surl);
        let me = self.me_in_call(&uuid::Uuid::new_v4().to_string());
        let video = media.sends_video();
        let offer = crate::sdp::to_ms(&media.offer().await?);
        tracing::trace!(sdp = %offer, "nabídka SDP");
        let (to, scenario) = match &target {
            tymy_core::CallTarget::EchoBot => {
                (calls::ECHO_BOT.to_owned(), Some("UserInitiatedTestCall"))
            }
            tymy_core::CallTarget::Person(p) => (p.as_str().to_owned(), None),
        };
        let service = session
            .services
            .call_service
            .clone()
            .unwrap_or_else(|| calls::DEFAULT_CALL_SERVICE.to_owned());
        let service = url::Url::parse(&service).map_err(|e| Error::Other(e.to_string()))?;
        let body = calls::conversation_body(&me, &cb, &offer, &self.call_client(), scenario, video);
        let conv = self
            .call_signal(reqwest::Method::POST, service, &body, &call_id)
            .await?;
        let leave = calls::link(&conv, "/links/leave")?;
        let add = calls::link(&conv, "/links/addParticipant")?;
        self.active.lock().await.push(ActiveCall {
            call_id: call_id.clone(),
            agent: cb.id().to_owned(),
            media,
            me: me.clone(),
            media_leg_id: String::new(),
            answered: false,
            hangup: Hangup::Leave(leave),
            group: false,
            joined: false,
        });
        // 1:1 chat má id ze dvou MRI bez prefixu, seřazených
        let mut ids = [
            self.profile.account_id.to_string(),
            to.rsplit(':').next().unwrap_or(&to).to_owned(),
        ];
        if matches!(target, tymy_core::CallTarget::Person(_)) {
            ids.sort();
        }
        let thread = format!("19:{}_{}@unq.gbl.spaces", ids[0], ids[1]);
        let body = calls::add_body(&me, &cb, &to, &thread);
        if let Err(e) = self
            .call_signal(reqwest::Method::POST, add, &body, &call_id)
            .await
        {
            let _ = self.hang_up(&call_id).await;
            return Err(e);
        }
        Ok(call_id)
    }

    async fn accept_call(&self, call_id: &str, media: Arc<dyn tymy_core::Media>) -> Result<()> {
        let r = {
            let mut ringing = self.ringing.lock().await;
            let i = ringing
                .iter()
                .position(|r| r.call_id == call_id)
                .ok_or_else(|| Error::Other("hovor už nezvoní".into()))?;
            ringing.remove(i)
        };
        let surl = self
            .calls_info
            .lock()
            .await
            .clone()
            .map(|i| i.surl)
            .ok_or_else(|| Error::Other("služba hovorů není připojená".into()))?;
        let acceptance = r
            .acceptance
            .clone()
            .ok_or_else(|| Error::Protocol("hovor nejde přijmout (chybí odkaz)".into()))?;
        let (offer, leg) = r
            .offer
            .clone()
            .ok_or_else(|| Error::Protocol("příchozí hovor bez nabídky SDP".into()))?;
        let answer = crate::sdp::to_ms(&media.answer(&crate::sdp::from_ms(&offer)).await?);
        let cb = calls::Callbacks::with_id(&surl, &r.agent);
        let me = self.me_in_call(&r.participant_id);
        let body = calls::acceptance_body(&me, &cb, &answer, &leg, media.sends_video());
        let resp = self
            .call_signal(reqwest::Method::POST, acceptance, &body, call_id)
            .await?;
        let call_leg = calls::link(&resp, "/callAcceptance/links/callLeg")
            .or_else(|_| calls::link(&resp, "/links/callLeg"))
            .unwrap_or(r.reject);
        self.active.lock().await.push(ActiveCall {
            call_id: r.call_id,
            agent: r.agent,
            media,
            me,
            media_leg_id: leg,
            answered: true,
            hangup: Hangup::CallLeg(call_leg),
            group: r.group,
            joined: false,
        });
        Ok(())
    }

    async fn hang_up(&self, call_id: &str) -> Result<()> {
        let c = {
            let mut active = self.active.lock().await;
            match active.iter().position(|c| c.call_id == call_id) {
                Some(i) => active.remove(i),
                None => return Ok(()),
            }
        };
        self.send_hangup(c).await
    }
    async fn reject_call(&self, call_id: &str) -> Result<()> {
        let r = {
            let mut ringing = self.ringing.lock().await;
            let i = ringing
                .iter()
                .position(|r| r.call_id == call_id)
                .ok_or_else(|| Error::Other("hovor už nezvoní".into()))?;
            ringing.remove(i)
        };
        self.call_signal(
            reqwest::Method::DELETE,
            r.reject,
            &calls::reject_body(),
            &r.call_id,
        )
        .await
        .map(|_| ())
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
