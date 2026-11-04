//! Real-time události.
//!
//! První verze používá HTTP long-poll endpoint zděděný z protokolu Skype
//! (endpoint → subscription → opakovaný poll). Web klient Teams dnes používá
//! WebSocket „trouter“, ten přijde později jako druhá implementace.

pub(crate) mod poll;
