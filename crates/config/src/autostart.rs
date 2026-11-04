//! Spouštění po přihlášení do systému (XDG autostart, `~/.config/autostart/tymy.desktop`).
//!
//! Zapíná se pro jednoho uživatele z okna aplikace; balíček nic do `/etc/xdg/autostart`
//! neinstaluje. Po přihlášení se Tymy spustí skryté v liště (`--hidden`).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

const FILE: &str = "tymy.desktop";

/// Parametr, se kterým se aplikace spustí po přihlášení (bez okna, jen v liště).
pub const HIDDEN_ARG: &str = "--hidden";

fn autostart_dir() -> Option<PathBuf> {
    directories::BaseDirs::new().map(|d| d.config_dir().join("autostart"))
}

/// Je spouštění po přihlášení zapnuté?
pub fn enabled() -> bool {
    autostart_dir().is_some_and(|d| d.join(FILE).exists())
}

/// Zapne (zapíše `.desktop` soubor s cestou k `exe`) nebo vypne spouštění po přihlášení.
pub fn set(on: bool, exe: &Path) -> Result<()> {
    let dir = autostart_dir().context("nelze určit konfigurační adresář")?;
    set_in(&dir, on, exe)
}

fn set_in(dir: &Path, on: bool, exe: &Path) -> Result<()> {
    let path = dir.join(FILE);
    if !on {
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e).with_context(|| format!("smazání {}", path.display())),
        }
        return Ok(());
    }
    std::fs::create_dir_all(dir).with_context(|| format!("vytvoření {}", dir.display()))?;
    std::fs::write(&path, entry(exe)).with_context(|| format!("zápis {}", path.display()))
}

fn entry(exe: &Path) -> String {
    // cesta v uvozovkách (může obsahovat mezery); `"`, `\`, `` ` `` a `$` se escapují
    let quoted: String = exe
        .to_string_lossy()
        .chars()
        .flat_map(|c| match c {
            '"' | '\\' | '`' | '$' => vec!['\\', c],
            c => vec![c],
        })
        .collect();
    format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=Tymy\n\
         Comment=Lehký klient Microsoft Teams pro Linux\n\
         Exec=\"{quoted}\" {HIDDEN_ARG}\n\
         Icon=tymy\n\
         Terminal=false\n\
         X-GNOME-Autostart-enabled=true\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_and_removes_entry() {
        let dir = std::env::temp_dir().join(format!("tymy-autostart-{}", std::process::id()));
        let exe = Path::new("/opt/my apps/tymy");
        set_in(&dir, true, exe).unwrap();
        let text = std::fs::read_to_string(dir.join(FILE)).unwrap();
        assert!(
            text.contains("Exec=\"/opt/my apps/tymy\" --hidden\n"),
            "{text}"
        );
        set_in(&dir, false, exe).unwrap();
        assert!(!dir.join(FILE).exists());
        set_in(&dir, false, exe).unwrap(); // vypnutí podruhé nevadí
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(entry(Path::new("/a\"b$c")).contains(r#"Exec="/a\"b\$c" --hidden"#));
    }
}
