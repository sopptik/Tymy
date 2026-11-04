//! Adresy služeb a identifikátory klienta.
//!
//! Hodnoty jsou převzaté z pozorování provozu web/desktop klienta Teams a z veřejně
//! dostupných nástrojů. **Nejde o dokumentované API**, proto je lze přepsat konfigurací
//! (viz `docs/protocol/README.md`). U každé hodnoty je uvedeno, jestli byla ověřena.

use serde::{Deserialize, Serialize};
use tymy_core::AccountKind;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Endpoints {
    /// OAuth2 autorita, např. `https://login.microsoftonline.com/organizations`.
    pub authority: String,
    /// Veřejný (public) client id, pod kterým se přihlašujeme.
    pub client_id: String,
    /// Scope access tokenu, který se vyměňuje za skypetoken.
    pub skype_scope: String,
    /// Endpoint výměny access tokenu za skypetoken + regionální adresy služeb.
    pub authz_url: String,
    /// Výchozí chat služba, pokud ji authz odpověď neuvede.
    pub chat_service: String,
    /// Návratová adresa přihlášení v prohlížeči (authorization code + PKCE).
    /// `http://localhost…` = kód převezme místní server, jinak adresu vloží uživatel.
    pub redirect_uri: String,
}

/// Teams desktop klient (first-party, public client, podporuje device code flow).
/// Ověřeno: používají ho veřejné nástroje (AADInternals, roadtx) pro device code přihlášení.
const TEAMS_DESKTOP_CLIENT_ID: &str = "1fec8e78-bce4-4aaf-ab1b-5451cc387264";

/// Návratová adresa přihlášení v prohlížeči: místní server Tymy (port se doplní).
/// Ověřeno 2026-11-04 s pracovním účtem. `…/oauth2/nativeclient` v systémovém prohlížeči
/// nefunguje: Microsoft adresu s kódem hned přesměruje na `/common/wrongplace`.
const LOOPBACK_REDIRECT: &str = "http://localhost";

impl Endpoints {
    pub fn defaults(kind: AccountKind) -> Self {
        match kind {
            AccountKind::Work => Self {
                authority: "https://login.microsoftonline.com/organizations".into(),
                client_id: TEAMS_DESKTOP_CLIENT_ID.into(),
                skype_scope: "https://api.spaces.skype.com/.default".into(),
                authz_url: "https://authsvc.teams.microsoft.com/v1.0/authz".into(),
                chat_service: "https://emea.ng.msg.teams.microsoft.com".into(),
                redirect_uri: LOOPBACK_REDIRECT.into(),
            },
            // NEOVĚŘENO: hodnoty pro osobní účty jsou hypotéza, kterou je nutné potvrdit
            // odchytem provozu teams.live.com (fáze 0, krok 1).
            AccountKind::Personal => Self {
                authority: "https://login.microsoftonline.com/consumers".into(),
                client_id: TEAMS_DESKTOP_CLIENT_ID.into(),
                skype_scope: "https://api.fl.spaces.skype.com/.default".into(),
                authz_url: "https://teams.live.com/api/auth/v1.0/authz/consumer".into(),
                chat_service: "https://msgapi.teams.live.com".into(),
                redirect_uri: LOOPBACK_REDIRECT.into(),
            },
        }
    }

    /// Výchozí hodnoty pro daný typ účtu, přepsané hodnotami z `overrides`
    /// (typicky načtenými z konfiguračního souboru).
    pub fn with_overrides(kind: AccountKind, overrides: &EndpointOverrides) -> Self {
        let mut e = Self::defaults(kind);
        let o = &overrides;
        macro_rules! apply {
            ($($f:ident),*) => { $( if let Some(v) = &o.$f { e.$f = v.clone(); } )* };
        }
        apply!(
            authority,
            client_id,
            skype_scope,
            authz_url,
            chat_service,
            redirect_uri
        );
        e
    }

    pub(crate) fn device_code_url(&self) -> String {
        format!(
            "{}/oauth2/v2.0/devicecode",
            self.authority.trim_end_matches('/')
        )
    }

    pub(crate) fn authorize_url(&self) -> String {
        format!(
            "{}/oauth2/v2.0/authorize",
            self.authority.trim_end_matches('/')
        )
    }

    pub(crate) fn token_url(&self) -> String {
        format!("{}/oauth2/v2.0/token", self.authority.trim_end_matches('/'))
    }

    /// Scopes žádané při přihlášení: OIDC kvůli id_tokenu (jméno, id účtu),
    /// `offline_access` kvůli refresh tokenu.
    pub(crate) fn login_scopes(&self) -> String {
        format!("openid profile offline_access {}", self.skype_scope)
    }
}

impl Default for Endpoints {
    fn default() -> Self {
        Self::defaults(AccountKind::Work)
    }
}

/// Volitelné přepisy jednotlivých položek [`Endpoints`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EndpointOverrides {
    pub authority: Option<String>,
    pub client_id: Option<String>,
    pub skype_scope: Option<String>,
    pub authz_url: Option<String>,
    pub chat_service: Option<String>,
    pub redirect_uri: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overrides_replace_only_given_fields() {
        let o = EndpointOverrides {
            client_id: Some("abc".into()),
            ..Default::default()
        };
        let e = Endpoints::with_overrides(AccountKind::Work, &o);
        assert_eq!(e.client_id, "abc");
        assert_eq!(
            e.authz_url,
            Endpoints::defaults(AccountKind::Work).authz_url
        );
    }

    #[test]
    fn oauth_urls() {
        let mut e = Endpoints::defaults(AccountKind::Work);
        e.authority = "https://login.example/tenant/".into();
        assert_eq!(
            e.token_url(),
            "https://login.example/tenant/oauth2/v2.0/token"
        );
        assert_eq!(
            e.device_code_url(),
            "https://login.example/tenant/oauth2/v2.0/devicecode"
        );
    }
}
