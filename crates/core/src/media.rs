//! Rozhraní médií hovoru (zvuk, obraz). Signalizace (`tymy-api`) jím vyjednává SDP
//! ve standardním tvaru WebRTC; přenos, kodeky a zařízení řeší implementace
//! (`tymy-media`, GStreamer `webrtcbin`).

use async_trait::async_trait;

use crate::Result;

#[async_trait]
pub trait Media: Send + Sync {
    /// Nabídka SDP pro odchozí hovor (už se všemi kandidáty ICE, bez trickle).
    async fn offer(&self) -> Result<String>;
    /// Odpověď na nabídku protistrany (příchozí hovor, přeVyjednání během hovoru).
    async fn answer(&self, offer: &str) -> Result<String>;
    /// Odpověď protistrany na naši nabídku.
    async fn accept_answer(&self, answer: &str) -> Result<()>;
    /// Posíláme obraz z kamery.
    fn sends_video(&self) -> bool;
}
