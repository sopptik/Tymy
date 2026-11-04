use async_trait::async_trait;

use crate::{
    AccountKind, Conversation, ConversationId, Event, Message, MessageId, Outgoing, Person,
    Profile, Result, SentMessage,
};

/// Jedna stránka historie zpráv (od nejnovější po nejstarší).
#[derive(Debug, Clone, PartialEq)]
pub struct MessagePage {
    pub messages: Vec<Message>,
    /// Neprůhledný kurzor na starší zprávy; `None` = začátek historie.
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

    /// Seznam konverzací seřazený od poslední aktivity.
    async fn conversations(&self) -> Result<Vec<Conversation>>;

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

    /// Hledání lidí ve vlastní organizaci (adresář).
    async fn search_people(&self, query: &str) -> Result<Vec<Person>>;

    /// Hledání lidí mimo organizaci podle přesné e-mailové adresy.
    async fn find_external(&self, email: &str) -> Result<Vec<Person>>;
}
