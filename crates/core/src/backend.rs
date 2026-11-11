use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::{
    AccountKind, CalendarEvent, Conversation, ConversationId, Event, EventReply, Message,
    MessageId, NewMeeting, Outgoing, Person, Profile, Result, SentMessage,
};

/// Jedna stránka historie zpráv (od nejnovější po nejstarší).
#[derive(Debug, Clone, PartialEq)]
pub struct MessagePage {
    pub messages: Vec<Message>,
    /// Neprůhledný kurzor na starší zprávy; `None` = začátek historie.
    pub older: Option<String>,
}

/// Část seznamu konverzací (od nejnovější aktivity).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ConversationPage {
    pub conversations: Vec<Conversation>,
    /// Neprůhledný kurzor na další (starší) část; `None` = seznam je celý.
    pub older: Option<String>,
}

/// Rozhraní, přes které UI a synchronizace mluví s jedním přihlášeným účtem.
///
/// Implementace musí sama řešit obnovu tokenů. Chyba [`crate::Error::ReauthRequired`]
/// znamená, že je nutné nové interaktivní přihlášení.
#[async_trait]
pub trait ChatBackend: Send + Sync {
    fn kind(&self) -> AccountKind;

    async fn profile(&self) -> Result<Profile>;

    /// Seznam konverzací seřazený od poslední aktivity (celý).
    async fn conversations(&self) -> Result<Vec<Conversation>>;

    /// Jen nejnověji aktivní konverzace (pro častou obnovu); výchozí = celý seznam.
    async fn recent_conversations(&self) -> Result<Vec<Conversation>> {
        self.conversations().await
    }

    /// `pages` stránek seznamu od začátku (`cursor = None`) nebo od kurzoru z předchozí
    /// části. Výchozí implementace nestránkuje: vrátí celý seznam najednou.
    async fn conversation_pages(
        &self,
        cursor: Option<&str>,
        pages: usize,
    ) -> Result<ConversationPage> {
        let _ = pages;
        Ok(match cursor {
            None => ConversationPage {
                conversations: self.conversations().await?,
                older: None,
            },
            Some(_) => ConversationPage::default(),
        })
    }

    /// Stránka historie. `cursor = None` načte nejnovější zprávy.
    async fn messages(
        &self,
        conversation: &ConversationId,
        cursor: Option<&str>,
    ) -> Result<MessagePage>;

    /// Stránka historie pro indexaci na pozadí: jako [`ChatBackend::messages`],
    /// ale ve větších stránkách (méně požadavků).
    async fn history(
        &self,
        conversation: &ConversationId,
        cursor: Option<&str>,
    ) -> Result<MessagePage> {
        self.messages(conversation, cursor).await
    }

    /// Odešle zprávu: text (může být prázdný), obrázky (nejdřív se nahrají) a případnou odpověď.
    async fn send(&self, conversation: &ConversationId, message: &Outgoing) -> Result<SentMessage>;

    /// Odešle prostou textovou zprávu.
    async fn send_text(&self, conversation: &ConversationId, text: &str) -> Result<SentMessage> {
        let message = Outgoing {
            text: text.to_owned(),
            ..Default::default()
        };
        self.send(conversation, &message).await
    }

    /// Přidá (`on`) nebo odebere moji reakci (`key` jako v [`crate::Reaction::key`]).
    async fn react(
        &self,
        conversation: &ConversationId,
        message: &MessageId,
        key: &str,
        on: bool,
    ) -> Result<()>;

    /// Přepíše obsah vlastní zprávy; `html` je hotový obsah (viz [`crate::text`]).
    async fn edit(
        &self,
        conversation: &ConversationId,
        message: &MessageId,
        html: &str,
    ) -> Result<()>;

    /// Smaže vlastní zprávu.
    async fn delete(&self, conversation: &ConversationId, message: &MessageId) -> Result<()>;

    /// Nahlásí serveru, že je konverzace přečtená po zprávu `message` (včetně);
    /// `client_id` je její `client_message_id`, pokud ho známe.
    async fn mark_read(
        &self,
        conversation: &ConversationId,
        message: &MessageId,
        client_id: Option<&str>,
    ) -> Result<()>;

    /// Počká na další dávku real-time událostí (long-poll). Může vrátit prázdný seznam
    /// při vypršení intervalu; volající pak jednoduše zavolá znovu.
    async fn poll_events(&self) -> Result<Vec<Event>>;

    /// Stáhne médium vložené do zprávy (obrázek z [`crate::text::InlineImage`]).
    async fn fetch_media(&self, url: &str) -> Result<Vec<u8>>;

    /// Založí chat s danými účastníky (kromě mě); vrátí jeho id. S jedním účastníkem 1:1 chat,
    /// s více skupinový (`topic` = jeho název).
    async fn create_chat(
        &self,
        members: &[crate::UserId],
        topic: Option<&str>,
    ) -> Result<ConversationId>;

    /// Profily lidí podle MRI (jména pro seznam chatů).
    async fn profiles(&self, mris: &[crate::UserId]) -> Result<Vec<Person>>;

    /// Členové chatu (MRI), včetně mě.
    async fn members(&self, conversation: &ConversationId) -> Result<Vec<crate::UserId>>;

    /// Přejmenuje skupinový chat (prázdný název = bez názvu).
    async fn set_topic(&self, conversation: &ConversationId, topic: &str) -> Result<()>;

    /// Přidá lidi do skupinového chatu.
    async fn add_members(
        &self,
        conversation: &ConversationId,
        members: &[crate::UserId],
    ) -> Result<()>;

    /// Odebere člověka ze skupinového chatu.
    async fn remove_member(
        &self,
        conversation: &ConversationId,
        member: &crate::UserId,
    ) -> Result<()>;

    /// Opustí skupinový chat.
    async fn leave(&self, conversation: &ConversationId) -> Result<()>;

    /// Hledání lidí ve vlastní organizaci (adresář).
    async fn search_people(&self, query: &str) -> Result<Vec<Person>>;

    /// Hledání lidí mimo organizaci podle přesné e-mailové adresy.
    async fn find_external(&self, email: &str) -> Result<Vec<Person>>;

    /// Oznámí ostatním v konverzaci, že uživatel právě píše.
    async fn typing(&self, conversation: &ConversationId) -> Result<()> {
        let _ = conversation;
        Ok(())
    }

    /// Stav (přítomnost) uživatelů. Výchozí = účet stav neposkytuje.
    async fn presence(
        &self,
        users: &[crate::UserId],
    ) -> Result<Vec<(crate::UserId, crate::Presence)>> {
        let _ = users;
        Ok(vec![])
    }

    /// Nastaví, čí stav chceme dostávat v událostech [`crate::Event::Presence`]
    /// (nahradí předchozí seznam).
    async fn watch_presence(&self, users: &[crate::UserId]) -> Result<()> {
        let _ = users;
        Ok(())
    }

    /// Ohlásí serveru vlastní stav tohoto klienta (dostupný / pryč), aby ho viděli ostatní.
    /// Je třeba opakovat, jinak stav po čase vyprší.
    async fn publish_presence(&self, availability: crate::Availability) -> Result<()> {
        let _ = availability;
        Ok(())
    }

    /// Ručně nastaví vlastní stav (má přednost před hlášeným automaticky);
    /// `None` = zrušit ruční stav a vrátit se k automatickému.
    async fn set_status(&self, status: Option<crate::Availability>) -> Result<()> {
        let _ = status;
        Err(crate::Error::Other("účet neumí nastavit stav".into()))
    }

    /// Události kalendáře, které zasahují do intervalu `from..to` (včetně výskytů
    /// opakovaných událostí), seřazené podle začátku. Výchozí = účet kalendář nemá.
    async fn calendar(&self, from: DateTime<Utc>, to: DateTime<Utc>) -> Result<Vec<CalendarEvent>> {
        let _ = (from, to);
        Ok(vec![])
    }

    /// Naplánuje schůzku a rozešle pozvánky; vrátí id události.
    async fn create_meeting(&self, meeting: &NewMeeting) -> Result<String> {
        let _ = meeting;
        Err(crate::Error::Other("účet neumí plánovat schůzky".into()))
    }

    /// Zruší schůzku, kterou organizuji (účastníci dostanou zrušení s `comment`).
    async fn cancel_meeting(&self, event_id: &str, comment: &str) -> Result<()> {
        let _ = (event_id, comment);
        Err(crate::Error::Other("účet neumí rušit schůzky".into()))
    }

    /// Odpoví na pozvánku (organizátor dostane odpověď).
    async fn reply_to_event(&self, event_id: &str, reply: EventReply) -> Result<()> {
        let _ = (event_id, reply);
        Err(crate::Error::Other(
            "účet neumí odpovídat na pozvánky".into(),
        ))
    }
}
