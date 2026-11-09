# Tymy

Lehký nativní klient Microsoft Teams pro Linux (Ubuntu a odvozené distribuce).
Jen to podstatné, jako kdysi Skype: chat, hovory, kalendář s připomínkami. Bez OneDrive
a dalších integrací (jedinou výjimkou je kalendář).
Žádné Teams aplikace ani integrace.

> ⚠️ Tymy používá interní, nedokumentovaná API Teams. Mohou se kdykoli změnit
> a jejich použití je v šedé zóně podmínek služby Microsoftu. Projekt není nijak
> spojen s Microsoftem.

## Stav

Použitelný chat: okno `tymy` a vývojářské CLI `tymy-cli`. Podrobný stav je v `plan.md`.

| Fáze | Obsah | Stav |
|---|---|---|
| 0 | Přihlášení, seznam chatů, historie, odesílání | ověřeno pro pracovní účet |
| 1a | Chat: cache, formátování, odpovědi, reakce, úpravy, hledání, více účtů, skupinové chaty | hotovo |
| 1b | Real-time (trouter), osobní účty | čeká na odchyt provozu; přihlášení v prohlížeči, hlášení přečtení a jména hotové |
| 1c | Balíček `.deb`, spouštění po přihlášení | hotovo |
| 2 | Hovory a videohovory (výzkum) | — |
| 3 | Kalendář, připomínky a plánování schůzek | hotovo, čeká na test |

## Struktura

```
crates/core   doménový model a trait ChatBackend (nezávislý na Microsoftu)
crates/api    vše, co mluví s Microsoftem: auth, chatsvc, long-poll
crates/config sdílená konfigurace (účty, přepisy endpointů)
crates/cli    tymy-cli pro ladění protokolu
crates/app    okno aplikace (iced)
docs/protocol poznámky k protokolu a návod na odchyt provozu
```

## Vývoj

Požadavky: Rust ≥ 1.92, běžící Secret Service (GNOME Keyring nebo KWallet).

```sh
cargo test --workspace
cargo run -p tymy                          # okno aplikace
cargo run -p tymy-cli -- login              # pracovní/školní účet
cargo run -p tymy-cli -- login --personal   # osobní účet (zatím neověřeno)
cargo run -p tymy-cli -- chats
cargo run -p tymy-cli -- messages 1         # pořadí z výpisu chats nebo id konverzace
cargo run -p tymy-cli -- send 1 Ahoj
cargo run -p tymy-cli -- watch
cargo run -p tymy-cli -- calendar --days 7  # události kalendáře
cargo run -p tymy-cli -- -vv chats          # ladění: zredigovaná těla odpovědí
```

Konfigurace je v `~/.config/tymy/` (`accounts.toml`, `settings.toml`, volitelně
`endpoints.toml`). Refresh tokeny jsou výhradně v Secret Service.

Připomínky kalendáře se nastavují v `settings.toml`:

```toml
[calendar]
reminders = true        # upozorňovat na události notifikací
reminder_minutes = 15   # kolik minut před začátkem
```

## Instalace (.deb)

```sh
packaging/build-deb.sh                      # → target/deb/tymy_<verze>_amd64.deb
sudo apt install ./target/deb/tymy_*_amd64.deb
```

Balíček obsahuje `tymy` (okno) a `tymy-cli`, spouštěč v menu aplikací a ikony.
Spouštění po přihlášení se zapíná v okně (menu účtů → „Spouštět po přihlášení“);
`tymy --hidden` spustí aplikaci jen v liště.
Závislosti na systémových knihovnách doplní `dpkg-shlibdeps`. Ověřeno instalací
na čistém Ubuntu 24.04 (Docker). Odinstalace: `sudo apt remove tymy`.
