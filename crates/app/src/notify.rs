//! Desktopové notifikace (freedesktop D-Bus).

use notify_rust::{Hint, Notification};

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
