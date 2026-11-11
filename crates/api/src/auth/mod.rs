//! Přihlášení a správa tokenů.
//!
//! Tok: OAuth2 (device code, nebo authorization code + PKCE v prohlížeči) → access token pro `skype_scope` → výměna na authsvc
//! za *skypetoken* + regionální adresy služeb. Skypetoken se pak posílá chat službě.

mod browser;
mod device_code;
mod skype;
mod store;
mod token;

pub use browser::{BrowserLogin, redeem_code, start_browser_login};
pub use device_code::{DeviceCode, poll_device_code, refresh, start_device_code};
pub(crate) use skype::trusted;
pub use skype::{RegionServices, SkypeSession, exchange_for_skype_token};
pub use store::{CredentialStore, MemoryStore, SecretServiceStore};
pub use token::{Claims, TokenSet};

use tymy_core::{AccountId, AccountKind, Error, Profile, Result};

/// Sestaví profil účtu z claimů id_tokenu (případně access tokenu).
pub fn profile_from_tokens(kind: AccountKind, tokens: &TokenSet) -> Result<Profile> {
    let claims = tokens
        .id_token
        .as_deref()
        .and_then(|t| Claims::decode(t).ok())
        .or_else(|| Claims::decode(&tokens.access_token).ok())
        .ok_or_else(|| Error::Auth("token neobsahuje čitelné claimy (chybí id_token)".into()))?;
    let account_id = claims
        .oid
        .clone()
        .or_else(|| claims.sub.clone())
        .ok_or_else(|| Error::Auth("token neobsahuje oid ani sub".into()))?;
    Ok(Profile {
        account_id: AccountId(account_id),
        kind,
        display_name: claims.name,
        username: claims.preferred_username.or(claims.upn),
        tenant_id: claims.tid,
    })
}
