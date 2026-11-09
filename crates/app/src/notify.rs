//! Desktopové notifikace (freedesktop D-Bus).

use notify_rust::{Hint, Notification, Timeout};

/// Zobrazí notifikaci a počká, co s ní uživatel udělá. `true` = klikl na ni (otevřít chat).
pub async fn show(title: String, body: String) -> bool {
    tokio::task::spawn_blocking(move || {
        let handle = Notification::new()
            .appname("Tymy")
            .summary(&title)
            .body(&escape_markup(&body))
            .icon("mail-message-new")
            .hint(Hint::Category("im.received".into()))
            .action("default", "Otevřít")
            .show();
        match handle {
            Ok(h) => {
                let mut clicked = false;
                h.wait_for_action(|action| clicked = action == "default");
                clicked
            }
            Err(e) => {
                tracing::warn!(error = %e, "notifikaci nelze zobrazit");
                false
            }
        }
    })
    .await
    .unwrap_or(false)
}

/// Co uživatel udělal s připomínkou události.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReminderChoice {
    /// Klik na připomínku: otevřít událost v okně.
    Open,
    /// Připojit se ke schůzce.
    Join,
    Snooze,
    /// Zavřeno.
    Dismissed,
}

/// Připomínka události: zůstane zobrazená, dokud ji uživatel nezavře; tlačítka Připojit
/// (u online schůzky) a Odložit.
pub async fn reminder(title: String, body: String, can_join: bool) -> ReminderChoice {
    tokio::task::spawn_blocking(move || {
        let mut n = Notification::new();
        n.appname("Tymy")
            .summary(&title)
            .body(&escape_markup(&body))
            .icon("appointment-soon")
            .hint(Hint::Category("x-tymy.calendar".into()))
            .timeout(Timeout::Never)
            .action("default", "Otevřít");
        if can_join {
            n.action("join", "Připojit");
        }
        n.action(
            "snooze",
            &format!("Odložit o {} min", crate::calendar::SNOOZE_MINUTES),
        );
        match n.show() {
            Ok(h) => {
                let mut choice = ReminderChoice::Dismissed;
                h.wait_for_action(|action| {
                    choice = match action {
                        "default" => ReminderChoice::Open,
                        "join" => ReminderChoice::Join,
                        "snooze" => ReminderChoice::Snooze,
                        _ => ReminderChoice::Dismissed,
                    }
                });
                choice
            }
            Err(e) => {
                tracing::warn!(error = %e, "připomínku nelze zobrazit");
                ReminderChoice::Dismissed
            }
        }
    })
    .await
    .unwrap_or(ReminderChoice::Dismissed)
}

/// Notifikační servery interpretují v těle jednoduché HTML; text zprávy proto escapujeme.
fn escape_markup(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_markup() {
        assert_eq!(escape_markup("a <b> & c"), "a &lt;b&gt; &amp; c");
    }
}
