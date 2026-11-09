//! Klient interních (nedokumentovaných) API Microsoft Teams.
//!
//! **Veškerý kód závislý na protokolu Microsoftu patří sem.** Ostatní crate
//! pracují jen s typy z `tymy-core`. Popis protokolu: `docs/protocol/README.md`.

pub mod auth;
mod backend;
pub mod calendar;
pub mod chat;
mod endpoints;
mod http;
pub mod media;
pub mod people;
mod realtime;

pub use backend::{TeamsBackend, complete_login};
pub use endpoints::{EndpointOverrides, Endpoints};
pub use http::{Http, redact};
pub use reqwest::Method;
