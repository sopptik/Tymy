use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::{DateTime, Duration, Utc};
use serde::Deserialize;
use tymy_core::{Error, Result};

/// Výsledek OAuth2 token endpointu.
#[derive(Clone)]
pub struct TokenSet {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub id_token: Option<String>,
    pub expires_at: DateTime<Utc>,
}

impl std::fmt::Debug for TokenSet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenSet")
            .field("access_token", &"<redacted>")
            .field(
                "refresh_token",
                &self.refresh_token.as_ref().map(|_| "<redacted>"),
            )
            .field("id_token", &self.id_token.as_ref().map(|_| "<redacted>"))
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

/// Surová odpověď token endpointu.
#[derive(Deserialize)]
pub(crate) struct TokenResponse {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub id_token: Option<String>,
    #[serde(default)]
    pub expires_in: Option<i64>,
}

impl From<TokenResponse> for TokenSet {
    fn from(r: TokenResponse) -> Self {
        Self {
            access_token: r.access_token,
            refresh_token: r.refresh_token,
            id_token: r.id_token,
            expires_at: Utc::now() + Duration::seconds(r.expires_in.unwrap_or(3600)),
        }
    }
}

/// Podmnožina claimů JWT, kterou používáme pro identifikaci účtu.
///
/// Podpis se **neověřuje**: token jsme právě dostali přímo od autority přes TLS
/// a claimy slouží jen k pojmenování účtu, ne k autorizaci.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Claims {
    pub oid: Option<String>,
    pub sub: Option<String>,
    pub tid: Option<String>,
    pub name: Option<String>,
    pub preferred_username: Option<String>,
    pub upn: Option<String>,
    /// Pro koho token je (ladění rozsahů pro další služby).
    pub aud: Option<String>,
    pub scp: Option<String>,
    pub appid: Option<String>,
}

impl Claims {
    pub fn decode(jwt: &str) -> Result<Self> {
        let payload = jwt
            .split('.')
            .nth(1)
            .ok_or_else(|| Error::Protocol("token není JWT".into()))?;
        let bytes = URL_SAFE_NO_PAD
            .decode(payload.trim_end_matches('='))
            .map_err(|e| Error::Protocol(format!("JWT payload: {e}")))?;
        serde_json::from_slice(&bytes).map_err(|e| Error::Protocol(format!("JWT claims: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jwt(payload: &str) -> String {
        format!(
            "{}.{}.sig",
            URL_SAFE_NO_PAD.encode(r#"{"alg":"none"}"#),
            URL_SAFE_NO_PAD.encode(payload)
        )
    }

    #[test]
    fn decodes_claims() {
        let c = Claims::decode(&jwt(
            r#"{"oid":"o1","tid":"t1","name":"Jan Novák","preferred_username":"jan@firma.cz","extra":1}"#,
        ))
        .unwrap();
        assert_eq!(c.oid.as_deref(), Some("o1"));
        assert_eq!(c.name.as_deref(), Some("Jan Novák"));
        assert_eq!(c.preferred_username.as_deref(), Some("jan@firma.cz"));
    }

    #[test]
    fn rejects_non_jwt() {
        assert!(Claims::decode("opaque-token").is_err());
    }

    #[test]
    fn debug_does_not_leak() {
        let t = TokenSet {
            access_token: "SECRET".into(),
            refresh_token: Some("SECRET2".into()),
            id_token: None,
            expires_at: Utc::now(),
        };
        let s = format!("{t:?}");
        assert!(!s.contains("SECRET"));
    }
}
