# Tymy

Lehký nativní klient Microsoft Teams pro Linux (Ubuntu a odvozené distribuce).
Jen to podstatné, jako kdysi Skype: chat, soubory, hovory, kalendář s připomínkami.
Žádné Teams aplikace ani integrace.

> ⚠️ Tymy používá interní, nedokumentovaná API Teams. Mohou se kdykoli změnit
> a jejich použití je v šedé zóně podmínek služby Microsoftu. Projekt není nijak
> spojen s Microsoftem.

## Stav

Prototyp: okno `tymy` (chaty, historie, odesílání) a vývojářské CLI `tymy-cli`.

| Fáze | Obsah | Stav |
|---|---|---|
| 0 | Přihlášení, seznam chatů, zprávy (CLI) | ověřeno pro pracovní účet; real-time chybí (trouter) |
| 1 | GUI (iced), soubory, notifikace, tray, `.deb` | prototyp okna hotový |
| 2 | Hovory a videohovory (výzkum) | — |
| 3 | Kalendář a připomínky schůzek | — |

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
cargo run -p tymy-cli -- -vv chats          # ladění: zredigovaná těla odpovědí
```

Konfigurace je v `~/.config/tymy/` (`accounts.toml`, volitelně `endpoints.toml`).
Refresh tokeny jsou výhradně v Secret Service.
