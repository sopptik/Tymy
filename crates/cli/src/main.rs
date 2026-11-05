//! `tymy-cli` – vývojářský nástroj pro ověření a ladění protokolu Teams.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Local, Utc};
use clap::{Parser, Subcommand};
use tymy_api::{Http, TeamsBackend, auth, complete_login};
use tymy_core::{
    AccountKind, ChatBackend, Conversation, ConversationId, Error, Event, Message, MessageKind,
    text::html_to_plain,
};

use tymy_config::{AccountRecord, Config};

#[derive(Parser)]
#[command(
    name = "tymy-cli",
    version,
    about = "Vývojářské CLI klienta Tymy (Teams pro Linux)"
)]
struct Cli {
    /// Účet: pořadí (od 1), e-mail nebo id. Výchozí je první účet.
    #[arg(short, long, global = true)]
    account: Option<String>,
    /// Vypisovat surová data jako JSON.
    #[arg(long, global = true)]
    json: bool,
    /// Podrobné logování (-v debug, -vv trace včetně zredigovaných těl odpovědí).
    #[arg(short, long, global = true, action = clap::ArgAction::Count)]
    verbose: u8,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Přihlásí účet pomocí kódu zadaného v prohlížeči.
    Login {
        /// Osobní Microsoft účet (Teams free) místo pracovního/školního.
        #[arg(long)]
        personal: bool,
        /// Konkrétní tenant (id nebo doména), pokud výchozí „organizations“ nestačí.
        #[arg(long, conflicts_with = "personal")]
        tenant: Option<String>,
    },
    /// Vypíše přihlášené účty.
    Accounts,
    /// Odhlásí účet a smaže jeho uložené přihlášení.
    Logout,
    /// Seznam chatů.
    Chats {
        #[arg(short = 'n', long, default_value_t = 30)]
        limit: usize,
    },
    /// Historie zpráv chatu.
    Messages {
        /// Id konverzace nebo pořadí z výpisu `chats`.
        chat: String,
        /// Kolik stránek historie načíst.
        #[arg(short, long, default_value_t = 1)]
        pages: usize,
    },
    /// Odešle textovou zprávu.
    Send {
        /// Id konverzace nebo pořadí z výpisu `chats`.
        chat: String,
        /// Text zprávy.
        #[arg(required = true, num_args = 1..)]
        text: Vec<String>,
    },
    /// Sleduje události v reálném čase (Ctrl+C ukončí).
    Watch,
    /// Ladění: surový GET na chat službu, např. `/v1/users/ME/conversations?pageSize=1`.
    Raw {
        /// Cesta včetně query, začíná `/`.
        path: String,
    },
    /// Ladění: požadavek na middle tier, např. `mt GET beta/users/...`.
    Mt {
        /// HTTP metoda (GET, POST, …).
        method: String,
        /// Cesta relativní k `…/api/mt/<region>/`.
        path: String,
        /// Volitelné JSON tělo.
        body: Option<String>,
    },
    /// Stáhne médium (obrázek) vložené do zprávy.
    Media {
        /// URL z atributu `src` obrázku (…asm.skype.com/…).
        url: String,
        /// Kam uložit.
        #[arg(short, long)]
        output: std::path::PathBuf,
    },
    /// Ladění: nahraje obrázek do media služby (AMS) s právem čtení pro chat. Zprávu
    /// NEODESÍLÁ; vypíše id objektu.
    Upload {
        /// Id chatu, který má mít k obrázku přístup.
        conversation: String,
        /// Soubor s obrázkem.
        file: std::path::PathBuf,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    init_logging(cli.verbose);
    let mut config = Config::load()?;

    match cli.command {
        Command::Login { personal, tenant } => login(&mut config, personal, tenant).await,
        Command::Accounts => {
            for (i, a) in config.accounts.accounts.iter().enumerate() {
                println!("{:>2}. {}", i + 1, a.label());
            }
            Ok(())
        }
        Command::Logout => {
            let record = config.find(cli.account.as_deref())?.clone();
            let store = auth::SecretServiceStore::new().await?;
            auth::CredentialStore::delete(&store, &record.id).await?;
            if let Some(dirs) = directories::ProjectDirs::from("", "", "tymy") {
                let path = dirs.data_dir().join("cache.sqlite");
                if path.exists() {
                    tymy_store::Store::open(&path, &store.cache_key().await?)?
                        .forget_account(&record.id)?;
                }
            }
            config.accounts.accounts.retain(|a| a.id != record.id);
            config.save()?;
            println!("Odhlášen: {}", record.label());
            Ok(())
        }
        Command::Chats { limit } => {
            let backend = backend(&config, cli.account.as_deref()).await?;
            let convs = visible_conversations(&backend).await?;
            if cli.json {
                return print_json(&convs.iter().take(limit).collect::<Vec<_>>());
            }
            for (i, c) in convs.iter().take(limit).enumerate() {
                print_conversation(i + 1, c);
            }
            Ok(())
        }
        Command::Messages { chat, pages } => {
            let backend = backend(&config, cli.account.as_deref()).await?;
            let id = resolve_chat(&backend, &chat).await?;
            let mut all = Vec::new();
            let mut cursor: Option<String> = None;
            for _ in 0..pages.max(1) {
                let page = backend.messages(&id, cursor.as_deref()).await?;
                all.extend(page.messages);
                cursor = page.older;
                if cursor.is_none() {
                    break;
                }
            }
            all.reverse(); // od nejstarší
            if cli.json {
                return print_json(&all);
            }
            for m in &all {
                print_message(m, false);
            }
            Ok(())
        }
        Command::Send { chat, text } => {
            let backend = backend(&config, cli.account.as_deref()).await?;
            let id = resolve_chat(&backend, &chat).await?;
            let sent = backend.send_text(&id, &text.join(" ")).await?;
            println!("Odesláno (clientmessageid {})", sent.client_message_id);
            Ok(())
        }
        Command::Raw { path } => {
            let backend = backend(&config, cli.account.as_deref()).await?;
            print_json(&backend.raw_get(&path).await?)
        }
        Command::Mt { method, path, body } => {
            let backend = backend(&config, cli.account.as_deref()).await?;
            let method: tymy_api::Method = method
                .to_ascii_uppercase()
                .parse()
                .context("neplatná HTTP metoda")?;
            let body: Option<serde_json::Value> = body
                .as_deref()
                .map(serde_json::from_str)
                .transpose()
                .context("tělo není platný JSON")?;
            print_json(&backend.mt_request(method, &path, body.as_ref()).await?)
        }
        Command::Media { url, output } => {
            let backend = backend(&config, cli.account.as_deref()).await?;
            let bytes = backend.fetch_media(&url).await?;
            std::fs::write(&output, &bytes)
                .with_context(|| format!("zápis {}", output.display()))?;
            println!("Uloženo {} B do {}", bytes.len(), output.display());
            Ok(())
        }
        Command::Upload { conversation, file } => {
            let backend = backend(&config, cli.account.as_deref()).await?;
            let bytes =
                std::fs::read(&file).with_context(|| format!("čtení {}", file.display()))?;
            let name = file
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "obrazek.png".into());
            let id = backend
                .upload_image(&conversation.as_str().into(), &bytes, &name)
                .await?;
            println!("Nahráno: {id}");
            Ok(())
        }
        Command::Watch => {
            let backend = backend(&config, cli.account.as_deref()).await?;
            watch(&backend, cli.json).await
        }
    }
}

fn init_logging(verbose: u8) {
    let default = match verbose {
        0 => "warn",
        1 => "info,tymy_api=debug",
        _ => "info,tymy_api=trace",
    };
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(default));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
}

async fn login(config: &mut Config, personal: bool, tenant: Option<String>) -> Result<()> {
    let kind = if personal {
        AccountKind::Personal
    } else {
        AccountKind::Work
    };
    let authority = tenant.map(|t| format!("https://login.microsoftonline.com/{t}"));
    let ep = config.endpoints(kind, authority.as_deref());
    let http = Http::new()?;
    let store = auth::SecretServiceStore::new()
        .await
        .context("Secret Service (GNOME Keyring/KWallet) není dostupná")?;

    let code = auth::start_device_code(&http, &ep).await?;
    match &code.message {
        Some(m) => eprintln!("{m}"),
        None => eprintln!(
            "Otevřete {} a zadejte kód {}",
            code.verification_uri, code.user_code
        ),
    }
    let tokens = auth::poll_device_code(&http, &ep, &code).await?;
    let profile = complete_login(kind, &tokens, &store).await?;

    // Ověříme, že výměna za skypetoken funguje hned, ne až při prvním použití.
    let record = AccountRecord::from_profile(&profile, authority);
    config.upsert(record.clone());
    config.save()?;
    let backend = TeamsBackend::new(profile, ep, http, Arc::new(store));
    let session = backend
        .session()
        .await
        .context("přihlášení proběhlo, ale výměna za skypetoken selhala")?;
    println!("Přihlášen: {}", record.label());
    println!("Chat služba: {}", session.chat_service);
    Ok(())
}

async fn backend(config: &Config, selector: Option<&str>) -> Result<TeamsBackend> {
    let record = config.find(selector)?;
    let ep = config.endpoints(record.kind, record.authority.as_deref());
    let store = auth::SecretServiceStore::new().await?;
    Ok(TeamsBackend::new(
        record.profile(),
        ep,
        Http::new()?,
        Arc::new(store),
    ))
}

/// Chaty bez kanálů týmů a systémových vláken.
async fn visible_conversations(backend: &TeamsBackend) -> Result<Vec<Conversation>> {
    Ok(backend
        .conversations()
        .await?
        .into_iter()
        .filter(Conversation::is_chat)
        .collect())
}

async fn resolve_chat(backend: &TeamsBackend, chat: &str) -> Result<ConversationId> {
    if let Ok(i) = chat.parse::<usize>() {
        let convs = visible_conversations(backend).await?;
        let c = i
            .checked_sub(1)
            .and_then(|i| convs.get(i))
            .with_context(|| format!("chat č. {i} neexistuje (je jich {})", convs.len()))?;
        return Ok(c.id.clone());
    }
    if !chat.contains(':') {
        bail!("{chat:?} není id konverzace (např. 19:…@thread.v2) ani pořadí");
    }
    Ok(chat.into())
}

async fn watch(backend: &TeamsBackend, json: bool) -> Result<()> {
    eprintln!("Sleduji události, Ctrl+C ukončí…");
    let mut backoff = Duration::from_secs(1);
    loop {
        let result = tokio::select! {
            r = backend.poll_events() => r,
            _ = tokio::signal::ctrl_c() => return Ok(()),
        };
        match result {
            Ok(events) => {
                backoff = Duration::from_secs(1);
                for e in events {
                    if json {
                        println!("{}", serde_json::to_string(&e)?);
                    } else {
                        print_event(&e);
                    }
                }
            }
            Err(e @ Error::ReauthRequired(_)) => return Err(e.into()),
            Err(e) => {
                let wait = match &e {
                    Error::RateLimited {
                        retry_after: Some(d),
                    } => *d,
                    _ => backoff,
                };
                eprintln!("! {e} (další pokus za {}s)", wait.as_secs());
                tokio::time::sleep(wait).await;
                backoff = (backoff * 2).min(Duration::from_secs(60));
            }
        }
    }
}

fn print_json<T: serde::Serialize>(v: &T) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(v)?);
    Ok(())
}

fn local_time(t: Option<DateTime<Utc>>) -> String {
    match t {
        Some(t) => {
            let t = t.with_timezone(&Local);
            if t.date_naive() == Local::now().date_naive() {
                t.format("%H:%M").to_string()
            } else {
                t.format("%d.%m. %H:%M").to_string()
            }
        }
        None => "—".into(),
    }
}

fn preview(html: &str, max: usize) -> String {
    let plain = html_to_plain(html).replace('\n', " ");
    if plain.chars().count() > max {
        format!("{}…", plain.chars().take(max).collect::<String>())
    } else {
        plain
    }
}

fn print_conversation(i: usize, c: &Conversation) {
    let unread = if c.has_unread() { "●" } else { " " };
    let last = c
        .last_message
        .as_ref()
        .filter(|m| m.is_user_content())
        .map(|m| preview(&m.content, 60))
        .unwrap_or_default();
    println!(
        "{i:>3}. {unread} {:<12} {}  {}\n       {}",
        local_time(c.last_activity),
        c.display_title(),
        last,
        c.id
    );
}

fn print_message(m: &Message, with_chat: bool) {
    let who = m.sender_name.as_deref().unwrap_or("?");
    let chat = if with_chat {
        format!("[{}] ", m.conversation_id)
    } else {
        String::new()
    };
    let time = local_time(m.arrived_at.or(m.composed_at));
    if m.deleted {
        println!("{chat}{time} {who}: (smazáno)");
        return;
    }
    match &m.kind {
        MessageKind::Event(t) | MessageKind::Unknown(t) => println!("{chat}{time} · {t}"),
        _ => {
            let edited = if m.edited_at.is_some() {
                " (upraveno)"
            } else {
                ""
            };
            println!("{chat}{time} {who}: {}{edited}", html_to_plain(&m.content));
        }
    }
    for a in &m.attachments {
        println!("        📎 {} {}", a.name, a.url.as_deref().unwrap_or(""));
    }
}

fn print_event(e: &Event) {
    match e {
        Event::NewMessage(m) => print_message(m, true),
        Event::MessageUpdated(m) => {
            print!("(změna) ");
            print_message(m, true);
        }
        Event::ConversationUpdated(id) => println!("· konverzace změněna: {id}"),
        Event::Typing {
            conversation_id,
            user,
        } => println!(
            "· {} píše v {conversation_id}",
            user.as_ref().map(|u| u.as_str()).unwrap_or("někdo")
        ),
        Event::Unknown { resource_type, .. } => {
            tracing::debug!(%resource_type, "neznámá událost");
        }
    }
}
