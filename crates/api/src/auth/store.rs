//! Bezpečné uložení refresh tokenů.

use std::collections::HashMap;

use async_trait::async_trait;
use tokio::sync::Mutex;
use tymy_core::{AccountId, Error, Result};

/// Úložiště refresh tokenů (jediné dlouhodobé tajemství klienta).
#[async_trait]
pub trait CredentialStore: Send + Sync {
    async fn load(&self, account: &AccountId) -> Result<Option<String>>;
    async fn save(&self, account: &AccountId, refresh_token: &str) -> Result<()>;
    async fn delete(&self, account: &AccountId) -> Result<()>;
}

const APP_ATTR: &str = "tymy";

/// Freedesktop Secret Service (GNOME Keyring, KWallet), ve Flatpaku přes portál.
pub struct SecretServiceStore {
    keyring: oo7::Keyring,
}

impl SecretServiceStore {
    pub async fn new() -> Result<Self> {
        let keyring = oo7::Keyring::new().await.map_err(storage_err)?;
        Ok(Self { keyring })
    }

    fn attributes(account: &AccountId) -> [(&'static str, &str); 3] {
        [
            ("application", APP_ATTR),
            ("kind", "refresh_token"),
            ("account", account.as_str()),
        ]
    }
}

fn storage_err(e: oo7::Error) -> Error {
    Error::Storage(format!("Secret Service: {e}"))
}

#[async_trait]
impl CredentialStore for SecretServiceStore {
    async fn load(&self, account: &AccountId) -> Result<Option<String>> {
        self.keyring.unlock().await.map_err(storage_err)?;
        let items = self
            .keyring
            .search_items(&Self::attributes(account))
            .await
            .map_err(storage_err)?;
        let Some(item) = items.into_iter().next() else {
            return Ok(None);
        };
        item.unlock().await.map_err(storage_err)?;
        let secret = item.secret().await.map_err(storage_err)?;
        let token = std::str::from_utf8(&secret)
            .map_err(|_| Error::Storage("uložený token není UTF-8".into()))?;
        Ok(Some(token.to_owned()))
    }

    async fn save(&self, account: &AccountId, refresh_token: &str) -> Result<()> {
        self.keyring.unlock().await.map_err(storage_err)?;
        self.keyring
            .create_item(
                &format!("Tymy – přihlášení účtu {account}"),
                &Self::attributes(account),
                refresh_token,
                true,
            )
            .await
            .map_err(storage_err)
    }

    async fn delete(&self, account: &AccountId) -> Result<()> {
        self.keyring
            .delete(&Self::attributes(account))
            .await
            .map_err(storage_err)
    }
}

/// Paměťové úložiště pro testy.
#[derive(Default)]
pub struct MemoryStore {
    tokens: Mutex<HashMap<AccountId, String>>,
}

#[async_trait]
impl CredentialStore for MemoryStore {
    async fn load(&self, account: &AccountId) -> Result<Option<String>> {
        Ok(self.tokens.lock().await.get(account).cloned())
    }

    async fn save(&self, account: &AccountId, refresh_token: &str) -> Result<()> {
        self.tokens
            .lock()
            .await
            .insert(account.clone(), refresh_token.to_owned());
        Ok(())
    }

    async fn delete(&self, account: &AccountId) -> Result<()> {
        self.tokens.lock().await.remove(account);
        Ok(())
    }
}
