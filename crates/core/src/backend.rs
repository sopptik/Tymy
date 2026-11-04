use async_trait::async_trait;

use crate::{
    AccountKind, Conversation, ConversationId, Event, Message, Person, Profile, Result, SentMessage,
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

    /// Odešle textovou zprávu. `text` je prostý text, implementace ho escapuje do HTML.
    async fn send_text(&self, conversation: &ConversationId, text: &str) -> Result<SentMessage>;

    /// Počká na další dávku real-time událostí (long-poll). Může vrátit prázdný seznam
    /// při vypršení intervalu; volající pak jednoduše zavolá znovu.
    async fn poll_events(&self) -> Result<Vec<Event>>;

    /// Stáhne médium vložené do zprávy (obrázek z [`crate::text::InlineImage`]).
    async fn fetch_media(&self, url: &str) -> Result<Vec<u8>>;

    /// Založí chat s danými účastníky (kromě mě); vrátí jeho id.
    async fn create_chat(&self, members: &[crate::UserId]) -> Result<ConversationId>;

    /// Hledání lidí ve vlastní organizaci (adresář).
    async fn search_people(&self, query: &str) -> Result<Vec<Person>>;

    /// Hledání lidí mimo organizaci podle přesné e-mailové adresy.
    async fn find_external(&self, email: &str) -> Result<Vec<Person>>;
}
