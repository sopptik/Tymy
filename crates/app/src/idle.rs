//! Nečinnost uživatele (myš a klávesnice) pro hlášení stavu „Pryč“.
//!
//! Zdroj se liší podle prostředí, proto se při prvním dotazu jednou rozpozná
//! ([`Source::detect`]) a dál se používá jen ten, který opravdu odpověděl:
//!
//! | Prostředí | Zdroj |
//! |---|---|
//! | X11 (MATE, Cinnamon, Xfce, KDE/GNOME na X11) | rozšíření MIT-SCREEN-SAVER |
//! | Wayland GNOME (Mutter) | D-Bus `org.gnome.Mutter.IdleMonitor.GetIdletime` |
//! | Wayland KDE a další | D-Bus `org.freedesktop.ScreenSaver.GetSessionIdleTime` |
//! | cokoli se systemd | logind `IdleHint` / `IdleSinceHint` relace |
//!
//! Na Waylandu se X11 nepoužívá, i když běží XWayland: ten vidí jen vstup do X11 oken.
//! Když neodpoví nic, nečinnost neznáme a aplikace hlásí „Dostupný“, dokud běží.

use std::sync::OnceLock;
use std::time::Duration;

use zbus::blocking::Connection;
use zbus::zvariant::OwnedValue;

/// Odkud bereme dobu nečinnosti.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// X11, rozšíření MIT-SCREEN-SAVER.
    X11,
    /// GNOME (Mutter), doba v milisekundách.
    Mutter,
    /// `org.freedesktop.ScreenSaver`; `millis` = KDE vrací milisekundy, specifikace sekundy.
    ScreenSaver { millis: bool },
    /// systemd-logind: příznak nečinnosti relace (nastavuje ho prostředí, hrubší).
    Logind,
}

/// Typ grafické relace podle proměnných prostředí.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Session {
    X11,
    Wayland,
    Unknown,
}

impl Session {
    /// `XDG_SESSION_TYPE`, jinak podle `WAYLAND_DISPLAY` / `DISPLAY`.
    pub fn from_env(session_type: Option<&str>, wayland: bool, x11: bool) -> Self {
        match session_type.map(str::to_ascii_lowercase).as_deref() {
            Some("wayland") => Self::Wayland,
            Some("x11") => Self::X11,
            _ if wayland => Self::Wayland,
            _ if x11 => Self::X11,
            _ => Self::Unknown,
        }
    }

    fn current() -> Self {
        let set = |k: &str| std::env::var_os(k).is_some_and(|v| !v.is_empty());
        Self::from_env(
            std::env::var("XDG_SESSION_TYPE").ok().as_deref(),
            set("WAYLAND_DISPLAY"),
            set("DISPLAY"),
        )
    }
}

/// Pořadí zdrojů, které má smysl v dané relaci zkoušet.
pub fn candidates(session: Session, kde: bool) -> Vec<Source> {
    let dbus = [
        Source::Mutter,
        Source::ScreenSaver { millis: kde },
        Source::Logind,
    ];
    match session {
        Session::X11 => std::iter::once(Source::X11).chain(dbus).collect(),
        // XWayland by hlásil jen vstup do X11 oken
        Session::Wayland => dbus.to_vec(),
        Session::Unknown => std::iter::once(Source::X11).chain(dbus).collect(),
    }
}

impl Source {
    /// Vybere první zdroj, který v tomto prostředí odpoví.
    fn detect() -> Option<Self> {
        let session = Session::current();
        let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
        let kde = desktop.to_ascii_uppercase().contains("KDE");
        let found = candidates(session, kde)
            .into_iter()
            .find(|s| s.query().is_some());
        match found {
            Some(source) => {
                tracing::info!(?session, %desktop, ?source, "zdroj nečinnosti uživatele")
            }
            None => tracing::warn!(
                ?session,
                %desktop,
                "nečinnost uživatele nejde zjistit, stav „Pryč“ se nebude hlásit"
            ),
        }
        found
    }

    fn query(self) -> Option<Duration> {
        match self {
            Source::X11 => x11_idle(),
            Source::Mutter => {
                let ms: u64 = call(
                    &Connection::session().ok()?,
                    "org.gnome.Mutter.IdleMonitor",
                    "/org/gnome/Mutter/IdleMonitor/Core",
                    "org.gnome.Mutter.IdleMonitor",
                    "GetIdletime",
                )?;
                Some(Duration::from_millis(ms))
            }
            Source::ScreenSaver { millis } => {
                let t: u32 = call(
                    &Connection::session().ok()?,
                    "org.freedesktop.ScreenSaver",
                    "/org/freedesktop/ScreenSaver",
                    "org.freedesktop.ScreenSaver",
                    "GetSessionIdleTime",
                )?;
                Some(if millis {
                    Duration::from_millis(t.into())
                } else {
                    Duration::from_secs(t.into())
                })
            }
            Source::Logind => logind_idle(),
        }
    }
}

fn call<T>(conn: &Connection, dest: &str, path: &str, iface: &str, method: &str) -> Option<T>
where
    T: serde::de::DeserializeOwned + zbus::zvariant::Type,
{
    conn.call_method(Some(dest), path, Some(iface), method, &())
        .ok()?
        .body()
        .deserialize()
        .ok()
}

fn x11_idle() -> Option<Duration> {
    use x11rb::connection::Connection as _;
    use x11rb::protocol::screensaver::ConnectionExt;
    let (conn, screen) = x11rb::connect(None).ok()?;
    let root = conn.setup().roots.get(screen)?.root;
    let info = conn.screensaver_query_info(root).ok()?.reply().ok()?;
    Some(Duration::from_millis(u64::from(info.ms_since_user_input)))
}

/// Nečinnost relace podle logind: `IdleHint` a čas, odkdy platí.
fn logind_idle() -> Option<Duration> {
    let conn = Connection::system().ok()?;
    let get = |name: &str| -> Option<OwnedValue> {
        conn.call_method(
            Some("org.freedesktop.login1"),
            "/org/freedesktop/login1/session/auto",
            Some("org.freedesktop.DBus.Properties"),
            "Get",
            &("org.freedesktop.login1.Session", name),
        )
        .ok()?
        .body()
        .deserialize()
        .ok()
    };
    let idle = bool::try_from(get("IdleHint")?).ok()?;
    if !idle {
        return Some(Duration::ZERO);
    }
    // mikrosekundy od epochy
    let since = u64::try_from(get("IdleSinceHint")?).ok()?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?;
    Some(now.saturating_sub(Duration::from_micros(since)))
}

/// Doba od poslední aktivity uživatele; `None` = v tomto prostředí ji nejde zjistit.
pub fn idle_time() -> Option<Duration> {
    static SOURCE: OnceLock<Option<Source>> = OnceLock::new();
    (*SOURCE.get_or_init(Source::detect))?.query()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_from_environment() {
        assert_eq!(
            Session::from_env(Some("wayland"), false, true),
            Session::Wayland
        );
        assert_eq!(Session::from_env(Some("X11"), true, true), Session::X11);
        // bez XDG_SESSION_TYPE (např. ssh -X, tty) rozhodnou proměnné displeje
        assert_eq!(Session::from_env(None, true, true), Session::Wayland);
        assert_eq!(Session::from_env(Some("tty"), false, true), Session::X11);
        assert_eq!(Session::from_env(None, false, false), Session::Unknown);
    }

    #[test]
    fn wayland_never_asks_x11() {
        let w = candidates(Session::Wayland, true);
        assert!(!w.contains(&Source::X11));
        assert_eq!(w[0], Source::Mutter);
        assert!(w.contains(&Source::ScreenSaver { millis: true }));
        assert_eq!(w.last(), Some(&Source::Logind));
        let x = candidates(Session::X11, false);
        assert_eq!(x[0], Source::X11);
        assert!(x.contains(&Source::ScreenSaver { millis: false }));
    }
}
