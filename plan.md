# Plán vývoje Tymy

Živý dokument. Aktualizovat při každé dokončené části nebo změně rozhodnutí.
Technické detaily protokolu jsou v `docs/protocol/README.md`.

## 1. Zadání a připomínky uživatele

Požadavky a rozhodnutí, jak je zadal uživatel:

- **Cíl:** nativní klient Microsoft Teams pro Linux, obdoba předchůdce Teams, Skype.
- **Platforma:** první iterace jen Ubuntu a odvozené distribuce (Kubuntu, Xubuntu, Mint, Pop!_OS…).
- **Rozsah a pořadí funkcí:**
  1. základní chat + posílání souborů,
  2. hlasové hovory a videohovory,
  3. kalendář a připomínky schůzek.
- **Mimo rozsah:** žádné Teams aplikace ani další integrace Microsoft aplikací.
- **Typy účtů:** pracovní/školní (M365) **i** osobní (Teams free).
- **Backend:** interní (nedokumentovaná) API Teams web klienta, ne Microsoft Graph.
- **Hovory:** pokus o nativní implementaci (výzkum), ne vložený web.
- **UI:** čistě rustový toolkit (iced / Slint) → zvolen **iced**.
- **Priorita (2026-11-04):** nejdřív okno aplikace, až potom trouter (real-time).
- **Zpětná vazba k prototypu (2026-11-04):** okno funguje, konverzace se zobrazují,
  „prototyp vypadá dobře“, odesílání zpráv funguje.
- **Obrázky (2026-11-04):** obrázky musí být vidět v konverzaci; po kliknutí otevřít originál, ne náhled.
- **Šifrování cache (2026-11-04):** zprávy v lokální cache šifrovat; cache se kvůli tomu nemaže,
  zůstává trvale (šifrovaná).
- **Posouvání (2026-11-04):** vlákno vždy posunout na nejnovější zprávu. Když uživatel ručně
  posune jinam a přijde zpráva, ukázat drobný overlay „nové zprávy“, po kliknutí posunout na ni.
  Týká se jen aktuální konverzace.
- **Reakce (2026-11-04):** reakce na zprávy musí být vidět.
- **Pracovní postup:** po každé změně aplikaci přebuildovat (a restartovat okno).
- **Logování (2026-11-04):** dočasné detailní logování pro snadný debug, s přepínačem
  na produkční provoz, který loguje jen chyby.
- **Hledání lidí (2026-11-04):** hledat lidi nejen z adresáře.
- **Týmy a chaty (2026-11-04):** oddělit týmy a chaty (zvoleno: dvě záložky). Chaty ručně
  řadit (zvoleno: připnutí + šipky); nepřipnuté podle data poslední komunikace.
- **Značky nepřečtených (2026-11-04):** na záložkách a u týmů ukázat, kde je něco nového.
- **Trouter (2026-11-04):** doladíme později. Do té doby zůstává obnova každých 5 s.
- **Ikona v liště (2026-11-04):** obálka je zavádějící → vlastní ikona (bublina chatu).
- **Design (2026-11-04):** přepínání vzhledu tmavý/světlý a celkově modernější vzhled.
- **Kopírování (2026-11-04):** možnost označit část textu zprávy a kopírovat; po označení
  automaticky do schránky.
- **Obrázky ze schránky (2026-11-04):** vkládat obrázky ze schránky; vložený obrázek musí
  jít odebrat.

## 2. Architektura (návrh, realizováno)

| Crate | Úloha |
|---|---|
| `crates/core` (`tymy-core`) | doménový model, trait `ChatBackend`, převody HTML ↔ text; nezávislé na Microsoftu |
| `crates/api` (`tymy-api`) | **veškerý** kód závislý na protokolu MS: auth, chatsvc, real-time |
| `crates/config` (`tymy-config`) | účty a přepisy endpointů v `~/.config/tymy` |
| `crates/cli` (`tymy-cli`) | vývojářské CLI pro ladění protokolu |
| `crates/app` (`tymy`) | okno aplikace v iced |

Zásady:
- UI závisí jen na `tymy-core`; změny API Microsoftu se opravují jen v `tymy-api`.
- Endpointy a client id lze přepsat v `~/.config/tymy/endpoints.toml` bez rekompilace.
- Refresh tokeny výhradně v Secret Service (GNOME Keyring / KWallet), tokeny se v logech redigují.
- Odkazy ze serveru se následují jen na stejný host (ochrana tokenu).

Přijaté riziko: interní API se může bez varování změnit a jeho použití je v šedé zóně
podmínek služby Microsoftu.

## 3. Stav (2026-11-04, aktualizováno)

| Oblast | Stav |
|---|---|
| Přihlášení pracovního účtu (device code) | ✅ ověřeno |
| Výměna za skypetoken, regionální chat služba | ✅ ověřeno |
| Seznam chatů, historie, stránkování | ✅ ověřeno |
| Odeslání zprávy | ✅ ověřeno (z okna) |
| Okno: seznam chatů, vlákno, psaní, přihlášení v okně | ✅ prototyp |
| Vložené obrázky (náhled + originál po kliknutí) | ✅ |
| Reakce na zprávy | ✅ zobrazení |
| Šifrovaná lokální cache (SQLCipher, klíč v Secret Service) | ✅ |
| Posouvání na nejnovější + overlay „Nové zprávy“ | ✅ |
| Logování debug/production (soubor, rotace) | ✅ |
| Hledání lidí v adresáři | ✅ |
| Hledání lidí mimo organizaci | ❌ endpoint vrací 400, potřeba HAR |
| Nový 1:1 chat z hledání | 🟡 prázdné vlákno ✅, založení první zprávou neověřeno |
| Záložky Chaty/Týmy, kanály po týmech, odznaky nepřečtených | ✅ |
| Ruční pořadí chatů (připnutí, ▲▼) | ✅ lokálně |
| Víceřádkové psaní (Shift+Enter) | ✅ |
| Desktopové notifikace, ikona v liště, zavření = skrytí | ✅ |
| Vzhled světlý/tmavý/podle systému, nový design | ✅ |
| Kopírování zprávy, výběr textu s automatickým kopírováním | ✅ |
| Vkládání obrázků ze schránky | 🟡 UI ✅, nahrání do AMS neověřeno proti produkci |
| Real-time přes long-poll (Skype) | ❌ `410 Gone`, vypnuto Microsoftem |
| Real-time náhrada | dočasně obnova seznamu chatů každých 5 s |
| Osobní účty | ❓ hodnoty jsou hypotéza, neověřeno |

## 4. Plán dalších kroků

### Fáze 1a: dokončení chatu (nepotřebuje odchyt provozu)
1. ✅ **Lokální cache (`crates/store`)**: šifrovaná (SQLCipher), konverzace, zprávy i obrázky;
   rychlý start, historie offline, fulltext bez diakritiky (FTS5). Zbývá: hledání v UI.
2. ✅ **Víceřádkové psaní:** Enter odešle, Shift+Enter nový řádek (iced `text_editor`).
3. ✅ **Desktopové notifikace** (D-Bus, `notify-rust`): nová zpráva mimo otevřený chat,
   klik otevře chat.
4. ✅ **Ikona v liště** (StatusNotifierItem, `ksni`, vlastní kreslená ikona) s počtem
   nepřečtených; zavření okna = skrytí.
5. **Lepší zobrazení zpráv:** ✅ obrázky, ✅ reakce, ✅ emoji jako text; zbývá: odkazy, citace a odpovědi,
   zmínky, posílání reakcí.
6. **Úpravy a mazání vlastních zpráv.**
7. **Přepínání účtů** v okně (podpora více účtů je už v konfiguraci).

### Fáze 1b: potřebuje odchyt provozu (HAR z web klienta)
1. **Trouter (WebSocket):** okamžité doručování zpráv, „píše…“, přítomnost. Nahradí 5s obnovu.
   ⏸ **Odloženo na později** (rozhodnutí uživatele 2026-11-04); potřeba HAR záznam z web klienta
   včetně WebSocket rámců (postup v `docs/protocol/README.md`). Nejpozději před fází 2
   (příchozí hovory chodí přes trouter).
2. **Hlášení přečtení** serveru (consumption horizon), aby se nepřečtené shodovaly s web klientem.
3. **Jména účastníků** 1:1 chatů z profilů (middle tier), místo procházení historie.
4. **Odesílání souborů:** pracovní účet = OneDrive „Microsoft Teams Chat Files“ + `properties.files`;
   osobní = AMS upload. Drag & drop, dialog, průběh.
5. **Osobní účty:** ověřit autoritu, client id, scope, authz a chat host.
6. **Přihlášení s PKCE** pro organizace, které blokují device code.

### Fáze 1c: distribuce
1. Balíček `.deb` (`cargo-deb`): `.desktop` soubor, ikony, autostart (XDG).
2. Test instalace na čistém Ubuntu 24.04.
3. Volitelně Flatpak později.

### Fáze 2: hovory a videohovory (výzkum s rozhodovacím bodem)
1. Výzkum (časově omezený, cca 2–4 týdny): odchyt signalizace 1:1 hovoru z web klienta.
   Média jsou standardní WebRTC (DTLS-SRTP, Opus, H.264/VP8); neznámá je proprietární signalizace.
2. Prototyp v `tymy-cli`: odchozí 1:1 audio hovor; média přes `webrtc-rs`, záloha `libwebrtc`;
   zvuk přes PipeWire (`cpal`).
3. **Rozhodovací bod:**
   - funguje → příchozí hovory (zvonění přes trouter), video (kamera `nokhwa`), ztlumení, výběr zařízení;
   - nefunguje → záloha: tlačítko „Zavolat / Připojit“ otevře hovor v prohlížeči.
4. Skupinové hovory a schůzky až po úspěšném 1:1.

### Fáze 3: kalendář a připomínky
1. Načtení událostí (pracovní: kalendář Teams/Graph se stejným přihlášením; osobní: Outlook.com).
2. Agenda den/týden, detail schůzky, tlačítko „Připojit“ (fáze 2 nebo prohlížeč).
3. Připomínky: lokální plánovač běžící v liště, notifikace X minut předem s „Připojit / Odložit“,
   přepočet po probuzení ze spánku.
4. Volitelně přijetí/odmítnutí pozvánky.

## 5. Moje doporučení k pořadí
1. Teď **fáze 1a** (cache, víceřádkové psaní, notifikace, lišta): bez závislosti na odchytu provozu
   a s největším přínosem pro každodenní použití.
2. **Trouter** je odložený na později; doladíme ho s HAR záznamem, nejpozději před fází 2
   (je předpokladem pro příchozí hovory).
3. Soubory a hlášení přečtení hned po trouteru (stejný odchyt provozu).
4. `.deb` balíček jakmile je chat použitelný na denní bázi, ještě před hovory.

8. ✅ **Posouvání vlákna:** vždy na nejnovější zprávu, overlay „Nové zprávy“ při čtení starších.
9. ✅ **Logování:** `--log=debug|production`, `TYMY_LOG`, `settings.toml`; `~/.local/state/tymy/logs`.
10. ✅ **Hledání lidí** v adresáři + otevření/založení 1:1 chatu. Zbývá: lidé mimo organizaci (HAR).
11. ✅ **Záložky Chaty / Týmy**, odznaky nepřečtených, **ruční pořadí** připnutých chatů.
12. ✅ **Design:** vlastní paleta, avatary, přepínač vzhledu (`settings.toml` → `theme`).
13. ✅ **Kopírování:** ⧉ celá zpráva, ⌶/dvojklik režim výběru, automatické kopírování výběru
    (schránka i primární výběr X11).
14. 🟡 **Obrázky ze schránky:** Ctrl+V, náhledy s ✕, Backspace; odeslání přes AMS — ověřit.

## 6. Ověřování
- `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` (CI).
- Unit testy parsování na anonymizovaných fixturách, integrační testy proti `wiremock`.
- Ruční E2E s reálným účtem po každé části; výsledky zapisovat do `docs/protocol/README.md`.
- Hovory: tymy ↔ oficiální web klient. Připomínky: schůzka za 5 min, i po uspání notebooku.

## 7. Otevřené otázky
- Kdo a kdy pořídí HAR záznam (pracovní i osobní účet)?
- Název projektu „Tymy“ je pracovní, případně změnit.
- Hlavní větev: zůstat na `master`, nebo přejmenovat na `main`?
- Zda mají `.idea/` soubory zůstat v repozitáři.
