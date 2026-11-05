//! Doménový model klienta Tymy.
//!
//! UI i úložiště závisí pouze na tomto crate, nikdy přímo na `tymy-api`.
//! Díky tomu změny v (nedokumentovaných) API Microsoftu zůstanou izolované.

mod backend;
mod error;
mod model;
pub mod text;

pub use backend::{ChatBackend, ConversationPage, MessagePage};
pub use error::{Error, Result};
pub use model::*;
