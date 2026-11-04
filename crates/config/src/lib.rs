//! Neveřejná konfigurace: seznam účtů, nastavení, přepisy endpointů a logování.
//! Tajemství jsou v Secret Service.

pub mod logging;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use tymy_api::{EndpointOverrides, Endpoints};
use tymy_core::{AccountId, AccountKind, Profile};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountRecord {
    pub id: AccountId,
    pub kind: AccountKind,
    pub display_name: Option<String>,
    pub username: Option<String>,
    pub tenant_id: Option<String>,
    /// Autorita použitá při přihlášení (např. konkrétní tenant).
    pub authority: Option<String>,
}

impl AccountRecord {
    pub fn from_profile(p: &Profile, authority: Option<String>) -> Self {
        Self {
            id: p.account_id.clone(),
            kind: p.kind,
            display_name: p.display_name.clone(),
            username: p.username.clone(),
            tenant_id: p.tenant_id.clone(),
            authority,
        }
    }

    pub fn profile(&self) -> Profile {
        Profile {
            account_id: self.id.clone(),
            kind: self.kind,
            display_name: self.display_name.clone(),
            username: self.username.clone(),
            tenant_id: self.tenant_id.clone(),
        }
    }

    pub fn label(&self) -> String {
        let name = self.display_name.as_deref().unwrap_or("?");
        match &self.username {
            Some(u) => format!("{name} <{u}> [{}]", self.kind),
            None => format!("{name} [{}]", self.kind),
        }
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Accounts {
    #[serde(default)]
    pub accounts: Vec<AccountRecord>,
}

/// `endpoints.toml`: volitelné přepisy pro každý typ účtu.
#[derive(Debug, Default, Deserialize)]
pub struct EndpointsFile {
    #[serde(default)]
    pub work: EndpointOverrides,
    #[serde(default)]
    pub personal: EndpointOverrides,
}

/// `settings.toml`: uživatelská nastavení aplikace.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Settings {
    /// Režim logování: `"debug"` (podrobně) nebo `"production"` (jen chyby).
    pub log: Option<logging::LogMode>,
}

pub struct Config {
    dir: PathBuf,
    pub accounts: Accounts,
    pub settings: Settings,
    endpoints: EndpointsFile,
}

impl Config {
    pub fn load() -> Result<Self> {
        let dirs = directories::ProjectDirs::from("", "", "tymy")
            .context("nelze určit domovský adresář")?;
        Self::load_from(dirs.config_dir())
    }

    pub fn load_from(dir: &Path) -> Result<Self> {
        let accounts = read_toml(&dir.join("accounts.toml"))?.unwrap_or_default();
        let endpoints = read_toml(&dir.join("endpoints.toml"))?.unwrap_or_default();
        let settings = read_toml(&dir.join("settings.toml"))?.unwrap_or_default();
        Ok(Self {
            dir: dir.to_owned(),
            accounts,
            settings,
            endpoints,
        })
    }

    pub fn save(&self) -> Result<()> {
        std::fs::create_dir_all(&self.dir)
            .with_context(|| format!("vytvoření {}", self.dir.display()))?;
        let path = self.dir.join("accounts.toml");
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, toml::to_string_pretty(&self.accounts)?)?;
        std::fs::rename(&tmp, &path).with_context(|| format!("zápis {}", path.display()))?;
        Ok(())
    }

    pub fn endpoints(&self, kind: AccountKind, authority: Option<&str>) -> Endpoints {
        let overrides = match kind {
            AccountKind::Work => &self.endpoints.work,
            AccountKind::Personal => &self.endpoints.personal,
        };
        let mut ep = Endpoints::with_overrides(kind, overrides);
        if let Some(a) = authority {
            ep.authority = a.to_owned();
        }
        ep
    }

    pub fn upsert(&mut self, record: AccountRecord) {
        self.accounts.accounts.retain(|a| a.id != record.id);
        self.accounts.accounts.push(record);
    }

    /// Najde účet podle id, e-mailu nebo pořadí (od 1); bez výběru vrátí jediný/první účet.
    pub fn find(&self, selector: Option<&str>) -> Result<&AccountRecord> {
        let all = &self.accounts.accounts;
        let Some(sel) = selector else {
            return all
                .first()
                .context("žádný účet, nejdřív spusťte `tymy-cli login`");
        };
        if let Ok(i) = sel.parse::<usize>()
            && let Some(a) = i.checked_sub(1).and_then(|i| all.get(i))
        {
            return Ok(a);
        }
        let matches: Vec<_> = all
            .iter()
            .filter(|a| {
                a.id.as_str() == sel
                    || a.username
                        .as_deref()
                        .is_some_and(|u| u.eq_ignore_ascii_case(sel))
            })
            .collect();
        match matches.as_slice() {
            [one] => Ok(one),
            [] => bail!("účet {sel:?} nenalezen (viz `tymy-cli accounts`)"),
            _ => bail!("výběr {sel:?} odpovídá více účtům"),
        }
    }
}

fn read_toml<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<Option<T>> {
    match std::fs::read_to_string(path) {
        Ok(s) => Ok(Some(
            toml::from_str(&s).with_context(|| format!("chyba v {}", path.display()))?,
        )),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("čtení {}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(id: &str, user: &str) -> AccountRecord {
        AccountRecord {
            id: id.into(),
            kind: AccountKind::Work,
            display_name: None,
            username: Some(user.into()),
            tenant_id: None,
            authority: None,
        }
    }

    #[test]
    fn roundtrip_and_find() {
        let dir = std::env::temp_dir().join(format!("tymy-test-{}", std::process::id()));
        let mut c = Config::load_from(&dir).unwrap();
        c.upsert(record("a", "jan@firma.cz"));
        c.upsert(record("b", "eva@firma.cz"));
        c.upsert(record("a", "jan@firma.cz")); // duplicita se nahradí
        c.save().unwrap();
        std::fs::write(
            dir.join("endpoints.toml"),
            "[personal]\nclient_id = \"custom\"\n",
        )
        .unwrap();

        std::fs::write(dir.join("settings.toml"), "log = \"production\"\n").unwrap();
        let c = Config::load_from(&dir).unwrap();
        assert_eq!(c.settings.log, Some(logging::LogMode::Production));
        assert_eq!(c.accounts.accounts.len(), 2);
        assert_eq!(c.find(None).unwrap().id.as_str(), "b");
        assert_eq!(c.find(Some("2")).unwrap().id.as_str(), "a");
        assert_eq!(c.find(Some("JAN@firma.cz")).unwrap().id.as_str(), "a");
        assert!(c.find(Some("nikdo")).is_err());
        assert_eq!(c.endpoints(AccountKind::Personal, None).client_id, "custom");
        assert_eq!(
            c.endpoints(AccountKind::Work, Some("https://login.example/t"))
                .authority,
            "https://login.example/t"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
