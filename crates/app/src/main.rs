//! Tymy – lehký Teams klient pro Linux.
//!
//! Parametry: `--log=debug|production` (viz `tymy_config::logging`), `--hidden` = spustit
//! jen v liště bez okna (spouštění po přihlášení, viz `tymy_config::autostart`).

mod app;
mod calendar;
mod clipboard;
mod mention;
mod notify;
mod state;
mod style;
mod tray;

use tymy_config::{autostart, logging};

fn main() -> iced::Result {
    let log_arg = std::env::args()
        .skip(1)
        .find_map(|a| a.strip_prefix("--log=").map(str::to_owned));
    let settings = tymy_config::Config::load()
        .map(|c| c.settings)
        .unwrap_or_default();
    let mode = match logging::resolve_mode(log_arg.as_deref(), settings.log) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("tymy: {e}");
            std::process::exit(2);
        }
    };
    // Guard musí žít do konce programu, jinak se nezapíšou poslední řádky logu.
    let _log = logging::init("tymy", mode, false);
    let hidden = std::env::args().any(|a| a == autostart::HIDDEN_ARG);

    iced::application(app::App::boot, app::App::update, app::App::view)
        .title(app::App::title)
        .subscription(app::App::subscription)
        .theme(app::App::theme)
        .window(iced::window::Settings {
            size: iced::Size::new(app::WINDOW_WIDTH, 720.0),
            icon: tray::window_icon(64),
            visible: !hidden,
            // WM_CLASS / app id = název .desktop souboru (ikona a seskupení v panelu)
            platform_specific: iced::window::settings::PlatformSpecific {
                application_id: "tymy".into(),
                ..Default::default()
            },
            ..Default::default()
        })
        // zavření okna řeší aplikace (skrytí do lišty, viz Msg::CloseRequested)
        .exit_on_close_request(false)
        .run()
}
