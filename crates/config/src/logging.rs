//! Logování: podrobný režim pro ladění a produkční režim jen s chybami.
//!
//! Volba režimu (od nejvyšší priority):
//! 1. parametr aplikace `--log=debug|production`,
//! 2. proměnná prostředí `TYMY_LOG`,
//! 3. `log = "…"` v `~/.config/tymy/settings.toml`,
//! 4. výchozí: `debug` pro vývojový build, `production` pro release build.
//!
//! `RUST_LOG` (pokud je nastavená) přebíjí filtr režimu pro jemné ladění.
//! Logy jdou do `~/.local/state/tymy/logs/` (denní rotace, 7 dní); v režimu
//! debug navíc na stderr. Adresář logů je čitelný jen pro vlastníka: v režimu
//! debug může log obsahovat (zredigovaná) těla odpovědí včetně textu zpráv.

use std::path::PathBuf;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling::{Builder, Rotation};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer, fmt};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogMode {
    /// Podrobné logy pro ladění (HTTP, stav aplikace, těla odpovědí).
    Debug,
    /// Jen chyby.
    Production,
}

impl LogMode {
    /// Výchozí režim podle typu buildu.
    pub fn build_default() -> Self {
        if cfg!(debug_assertions) {
            LogMode::Debug
        } else {
            LogMode::Production
        }
    }

    /// Filtr pro `tracing` (když není nastavená `RUST_LOG`).
    pub fn filter(self) -> &'static str {
        match self {
            LogMode::Production => "error",
            LogMode::Debug => concat!(
                "info,",
                "tymy=debug,tymy_api=trace,tymy_store=debug,tymy_config=debug,tymy_cli=debug,",
                // knihovny vykreslování jsou upovídané a pro ladění klienta nezajímavé
                "wgpu_core=warn,wgpu_hal=error,naga=warn,iced_wgpu=warn,cosmic_text=warn,",
                "zbus=warn,hyper_util=info"
            ),
        }
    }
}

impl FromStr for LogMode {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "debug" | "dev" | "detail" => Ok(LogMode::Debug),
            "production" | "prod" | "error" | "errors" => Ok(LogMode::Production),
            other => Err(format!(
                "neznámý režim logování {other:?} (povoleno: debug, production)"
            )),
        }
    }
}

impl std::fmt::Display for LogMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            LogMode::Debug => "debug",
            LogMode::Production => "production",
        })
    }
}

/// Určí režim z parametru, `TYMY_LOG` a nastavení (v tomto pořadí).
pub fn resolve_mode(arg: Option<&str>, setting: Option<LogMode>) -> Result<LogMode, String> {
    if let Some(a) = arg {
        return a.parse();
    }
    if let Ok(env) = std::env::var("TYMY_LOG")
        && !env.trim().is_empty()
    {
        return env.parse();
    }
    Ok(setting.unwrap_or_else(LogMode::build_default))
}

/// Udržuje logovací vlákno naživu; musí žít do konce programu.
pub struct LogGuard {
    _file: Option<WorkerGuard>,
    pub file_dir: Option<PathBuf>,
    pub mode: LogMode,
}

/// Adresář logů: `~/.local/state/tymy/logs`.
pub fn log_dir() -> Option<PathBuf> {
    let dirs = directories::ProjectDirs::from("", "", "tymy")?;
    let base = dirs
        .state_dir()
        .map(|d| d.to_owned())
        .unwrap_or_else(|| dirs.data_local_dir().to_owned());
    Some(base.join("logs"))
}

/// Inicializuje globální logování. `app` je předpona souborů (`tymy`, `tymy-cli`).
/// `stderr`: vypisovat i na stderr (v režimu debug vždy).
pub fn init(app: &str, mode: LogMode, stderr: bool) -> LogGuard {
    let filter =
        || EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(mode.filter()));

    let (file_layer, guard, file_dir) =
        match log_dir().and_then(|dir| open_file(&dir, app).map(|w| (dir, w))) {
            Some((dir, appender)) => {
                let (writer, guard) = tracing_appender::non_blocking(appender);
                let layer = fmt::layer()
                    .with_writer(writer)
                    .with_ansi(false)
                    .with_target(true)
                    .with_thread_names(true)
                    .with_filter(filter());
                (Some(layer), Some(guard), Some(dir))
            }
            None => (None, None, None),
        };
    let stderr_layer = (stderr || mode == LogMode::Debug).then(|| {
        fmt::layer()
            .with_writer(std::io::stderr)
            .with_target(true)
            .with_filter(filter())
    });

    // Opakovaná inicializace (testy) není chyba.
    let _ = tracing_subscriber::registry()
        .with(file_layer)
        .with(stderr_layer)
        .try_init();

    install_panic_hook();
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        %mode,
        log_dir = ?file_dir,
        "logování spuštěno"
    );
    LogGuard {
        _file: guard,
        file_dir,
        mode,
    }
}

fn open_file(
    dir: &std::path::Path,
    app: &str,
) -> Option<tracing_appender::rolling::RollingFileAppender> {
    use std::os::unix::fs::PermissionsExt;
    if let Err(e) = std::fs::create_dir_all(dir)
        .and_then(|_| std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)))
    {
        eprintln!("tymy: nelze vytvořit adresář logů {}: {e}", dir.display());
        return None;
    }
    match Builder::new()
        .rotation(Rotation::DAILY)
        .filename_prefix(app)
        .filename_suffix("log")
        .max_log_files(7)
        .build(dir)
    {
        Ok(a) => Some(a),
        Err(e) => {
            eprintln!("tymy: nelze otevřít log v {}: {e}", dir.display());
            None
        }
    }
}

/// Pády zalogovat jako chybu (i v produkčním režimu), pak pokračovat výchozím hookem.
fn install_panic_hook() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let location = info
            .location()
            .map(|l| format!("{}:{}", l.file(), l.line()))
            .unwrap_or_default();
        let payload = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_default();
        tracing::error!(%location, %payload, "pád aplikace (panic)");
        default(info);
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_modes() {
        assert_eq!("debug".parse::<LogMode>().unwrap(), LogMode::Debug);
        assert_eq!("PROD".parse::<LogMode>().unwrap(), LogMode::Production);
        assert!("verbose".parse::<LogMode>().is_err());
    }

    #[test]
    fn argument_wins_over_setting() {
        assert_eq!(
            resolve_mode(Some("production"), Some(LogMode::Debug)).unwrap(),
            LogMode::Production
        );
        assert!(resolve_mode(Some("nesmysl"), None).is_err());
    }

    #[test]
    fn production_logs_only_errors() {
        assert_eq!(LogMode::Production.filter(), "error");
        assert!(LogMode::Debug.filter().contains("tymy_api=trace"));
    }
}
