use std::time::Duration;

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Přihlášení vypršelo nebo bylo odvoláno, uživatel se musí přihlásit znovu.
    #[error("vyžadováno nové přihlášení: {0}")]
    ReauthRequired(String),

    #[error("chyba autentizace: {0}")]
    Auth(String),

    #[error("server vrátil HTTP {status}: {body}")]
    Http { status: u16, body: String },

    #[error("překročen limit požadavků, zkuste znovu za {retry_after:?}")]
    RateLimited { retry_after: Option<Duration> },

    /// Odpověď neodpovídá očekávanému formátu, typicky změna API ze strany Microsoftu.
    #[error("neočekávaný formát odpovědi: {0}")]
    Protocol(String),

    #[error("síťová chyba: {0}")]
    Network(String),

    #[error("úložiště: {0}")]
    Storage(String),

    #[error("{0}")]
    Other(String),
}

impl Error {
    /// Má smysl požadavek zopakovat (po prodlevě)?
    pub fn is_transient(&self) -> bool {
        match self {
            Error::Network(_) | Error::RateLimited { .. } => true,
            Error::Http { status, .. } => *status >= 500,
            _ => false,
        }
    }
}
