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
  Upřesnění (2026-11-09): „nechci onedrive ani jiné integrace, výjimkou je kalendář“, pak
  po vysvětlení, že Teams soubory z pracovního účtu ukládá jen do OneDrive: „povol OneDrive
  jen pro odesílání souborů“. Povolené integrace jsou tedy jen dvě:
  - kalendář (čtení přes middle tier, zápis přes Graph);
  - OneDrive výhradně pro odeslání souboru do chatu, stejně jako oficiální klient: nahrát do
    „Microsoft Teams Chat Files“, nasdílet účastníkům, poslat odkaz. Žádné procházení
    OneDrive, synchronizace ani jiné funkce.
  Obrázky ze schránky jdou dál přes vlastní úložiště Teams (AMS). Přijaté soubory se
  otevírají odkazem v prohlížeči.
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
- **Šířka bublin (2026-11-05):** „musí být ty bubliny tak cik-cak … tabulka je zarovnaná do
  řádků uvnitř buněk, přitom by se vše vešlo na jednu řádku, kdyby nebyly bubliny tak úzké“
  (celá obrazovka, Full HD). Řešení: šířka bubliny podle místa ve vlákně (320–1100 px místo
  pevných 560), sloupce tabulek podle obsahu.
- **Tmavý vzhled (2026-11-05):** „u tmavého vzhledu by měla být moje konverzace taky tmavší“
  → vlastní bubliny v tmavém vzhledu tlumeně tmavě tyrkysové se světlým textem,
  odkazy světle tyrkysové; světlý vzhled beze změny. Následně „ještě bych trochu ztmavil“
  → `#14373E`, potvrzeno: „takhle je to dobré“.
- **Hlavička web klienta (2026-11-05):** „zkus to poslat s hlavičkou webových teams a uvidíme
  jestli je to tím“ → media služba přijme obrázek jen s `x-ms-client-version` web klienta.
- **Souhrnný test (2026-11-05):** „bod 1 se zatím zdá že funguje vše jak má“ (reakce, úpravy,
  mazání, odpovědi, obrázky, nové chaty, hlášení přečtení, citace, účty, autostart, `.deb`).
- **Obrázek ze schránky (2026-11-05):** „obrázek ze schránky už jde odeslat“ → bod 14 ✅.
- **Kalendář (2026-11-09):** „přidej kalendář. chci aby notifikoval schůzky a jiné události
  z kalendáře.“ a „tlačítko kalendáře bych přidal jako další menu pod Týmy a Chaty jako další
  řádek“ → záložka Kalendář na druhém řádku pod Chaty/Týmy, připomínky notifikací (fáze 3).
  Dále: „chci na ikonce ve status baru vidět že je notifikace schůzky … stejně jako je vidět
  nový chat měla by být vidět i schůzka. po najetí myši by mělo být vidět že se jedná
  o schůzku a ne nový chat“ → ikona v liště má u připomenuté schůzky oranžové hodiny vpravo
  dole (nepřečtené chaty zůstávají červenou tečkou vpravo nahoře), popisek „Připomínka
  schůzky: … v 10:00 (za 12 min)“ a položku v menu ikony; trvá i po zmizení notifikace.
- **Smazání hledání (2026-11-09):** „přidej do políčka vyhledávání ikonku na smazání obsahu
  vstupního políčka“ → křížek ✕ vpravo v poli (jen když není prázdné), smaže dotaz a vrátí
  kurzor do pole.
- **Barevné emoji (2026-11-09):** „mohli by být smailíci barevní?“ → emoji v textu (zprávy,
  citace, náhledy v seznamu, reakce, výsledky hledání, psací pole) se kreslí fontem Noto Color
  Emoji; jinak je iced bral z prvního fontu s glyfem, který byl černobílý. V psacím poli přes
  highlighter `text_editor`.
- **Samozhojitelné chyby (2026-11-10):** „mohla by chyba která je způsobená chybějícím
  připojením k síti zmizet po opětovném navázání připojení? resp všechny chyby které se opraví
  samy aby zmizely“ → přechodné chyby (síť, 429, 5xx) z činností na pozadí (obnova seznamu
  chatů, načítání zpráv a starších chatů, hledání lidí) zmizí samy po další úspěšné odpovědi
  serveru (obnova běží každých 5 s). Chyby akcí uživatele (odeslání, úprava, reakce, správa
  chatu, schůzky) zůstávají, dokud je nezavře: samy se neopraví. Chyby kalendáře už mizely
  po úspěšném načtení dřív.

- **Hledání (2026-11-04):** „hledání vyřeš pomocí indexace nebo nějakého fulltext enginu
  na způsob OpenSearch, pokud to má smysl“. Rozhodnutí: OpenSearch/Elasticsearch je serverová
  služba (JVM, stovky MB RAM), pro desktopového klienta nevhodná. Tantivy (Lucene v Rustu) by
  ukládal index nešifrovaně mimo SQLCipher a češtinu stejně neumí lépe (nemá český stemmer).
  Zvolen FTS5 přímo v šifrované cache + indexace historie na pozadí. Přechod na Tantivy zvážit,
  kdyby FTS5 nestačil (řazení podle relevance, překlepy).

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
| Nový 1:1 chat z hledání | ✅ ověřeno uživatelem 2026-11-05 |
| Nový skupinový chat | ✅ ověřeno uživatelem 2026-11-05 |
| Záložky Chaty/Týmy, kanály po týmech, odznaky nepřečtených | ✅ |
| Ruční pořadí chatů (připnutí, ▲▼) | ✅ lokálně |
| Víceřádkové psaní (Shift+Enter) | ✅ |
| Desktopové notifikace, ikona v liště, zavření = skrytí | ✅ |
| Vzhled světlý/tmavý/podle systému, nový design | ✅ |
| Kopírování zprávy, výběr textu s automatickým kopírováním | ✅ |
| Vkládání a odesílání obrázků ze schránky | ✅ (s `x-ms-client-version` web klienta) |
| Formátované zprávy (odkazy, kód, seznamy, tabulky, citace, zmínky) | ✅ ověřeno na reálných zprávách |
| Odpověď na zprávu | ✅ ověřeno uživatelem 2026-11-05 |
| Reakce, úprava a mazání vlastních zpráv | ✅ ověřeno uživatelem 2026-11-05 |
| Hledání ve zprávách + indexace historie na pozadí | ✅ |
| Přepínání účtů, přidání a odhlášení účtu | ✅ ověřeno se dvěma pracovními účty (ověřeno uživatelem 2026-11-05) |
| Nepřečtené v neaktivních účtech (menu, odznak u jména, lišta) | ✅ |
| Seznam konverzací po stránkách (>200 chatů) | ✅ líně: při startu jen první stránka (200); starší po dorolování na konec, celý seznam při otevření Týmů nebo hledání; po 5 min obnova načtených stránek, jinak nejnovější stránka (ladění: `TYMY_CONVERSATION_PAGE_SIZE`) |
| Notifikace z neaktivních účtů (klik přepne účet) | ✅ ověřeno uživatelem 2026-11-05 |
| Přihlášení v prohlížeči (PKCE, `http://localhost`) | ✅ ověřeno (druhý pracovní účet) |
| Opuštěné týmy a kanály bez přístupu (403) | ✅ skryté |
| Balíček `.deb`, instalace na čistém Ubuntu 24.04 | ✅ (Docker + Xvfb) |
| Hlášení přečtení serveru | ✅ ověřeno uživatelem 2026-11-05 |
| Skok z citace odpovědi na původní zprávu | ✅ ověřeno uživatelem 2026-11-05 |
| Real-time přes long-poll (Skype) | ❌ `410 Gone`, vypnuto Microsoftem |
| Real-time náhrada | dočasně obnova seznamu chatů každých 5 s |
| Osobní účty | ❓ hodnoty jsou hypotéza, neověřeno |
| Kalendář: agenda 14 dní, detail, „Připojit“, chat schůzky | ✅ (middle tier, ověřeno na obou pracovních účtech) |
| Připomínky událostí (všechny účty, Připojit / Odložit) | 🟡 hotovo, čeká na test uživatelem |

## 3a. Psaní a správa chatů (2026-11-09, zadání „naimplementuj body 1-5 a každý dej do
samostatného commitu“)
1. ✅ **Koncepty po chatech:** psací pole patří otevřenému chatu; rozepsaný text, vložené
   obrázky a odpověď se při přepnutí odloží a po návratu vrátí (dřív text přešel do jiného
   chatu). V seznamu chatů „Koncept: …“ červeně. Neodeslaná zpráva se při chybě vrátí do
   konceptu svého chatu, i když je otevřený jiný. Jen v paměti (ne po restartu).
2. 🟡 **@zmínky při psaní:** „@“ + začátek jména nabídne lidi chatu (členové z chat služby
   + profily, záloha odesílatelé zpráv; bez diakritiky, „novot“ najde Novotného), ↑↓, Enter/Tab,
   Esc, klik. Odesílá se `<span itemtype="http://schema.skype.com/Mention" itemid="N">` a
   `properties.mentions` (JSON v řetězci: `@type`, `itemid`, `mri`, `mentionType: person`,
   `displayName`). Nabídka ověřena v okně; odeslání zmínky čeká na test uživatelem.
   Ladění: `TYMY_OPEN_CHAT=… TYMY_COMPOSE="Ahoj @mat"` (jen vyplní pole).
3. 🟡 **Formátování při psaní** (Markdown, `crates/core/src/text/markdown.rs`): `**tučně**`,
   `_kurzíva_`, `~~škrtnuté~~`, `` `kód` ``, bloky ```` ``` ````, odrážky `- `, číslované
   seznamy (od dvou řádků, ať „9. 11. máme poradu“ zůstane textem), citace `> `, odkazy
   `[text](url)` a holé adresy jako `<a>`. Úprava zprávy převede HTML zpět na Markdown,
   takže formátování přežije (zmínky zůstanou jako text „@Jméno“). Nápověda v psacím poli.
   Odeslání čeká na test uživatelem (jak to zobrazí oficiální Teams, hlavně `<pre>`).
4. ✅ **Výběr emoji:** tlačítko ☺ vedle odeslání, záložky kategorií (smajlíci, gesta, srdce
   a symboly, příroda a jídlo, věci) a naposledy použité (v paměti), vloží na místo kurzoru,
   Esc zavře. Barevný font Noto Color Emoji (jinak část emoji černobíle). Ladění: `TYMY_EMOJI=1`.
5. 🟡 **Správa skupinových chatů:** tlačítko „⚙ Členové“ v hlavičce skupinového chatu otevře
   nastavení místo vlákna: přejmenování, členové s profily (načtení ověřeno: 21 členů),
   přidání lidí hledáním v adresáři, odebrání (potvrzení druhým klikem), opuštění chatu
   (potvrzení). Chat služba: `PUT /v1/threads/{id}/properties?name=topic` `{"topic"}`,
   `PUT /v1/threads/{id}/members/{mri}` `{"role": "Admin"}`, `DELETE …/members/{mri}`
   (opuštění = vlastní MRI). Endpointy podle Skype/Teams API bez HAR, změny na serveru čekají
   na test uživatelem. Ladění: `TYMY_OPEN_CHAT=<id> TYMY_CHAT_PANEL=1`.

## 4. Plán dalších kroků

### Fáze 1a: dokončení chatu (nepotřebuje odchyt provozu)
1. ✅ **Lokální cache (`crates/store`)**: šifrovaná (SQLCipher), konverzace, zprávy i obrázky;
   rychlý start, historie offline, fulltext bez diakritiky (FTS5).
2. ✅ **Víceřádkové psaní:** Enter odešle, Shift+Enter nový řádek (iced `text_editor`).
3. ✅ **Desktopové notifikace** (D-Bus, `notify-rust`): nová zpráva mimo otevřený chat,
   klik otevře chat.
4. ✅ **Ikona v liště** (StatusNotifierItem, `ksni`, vlastní kreslená ikona) s počtem
   nepřečtených; zavření okna = skrytí.
5. **Lepší zobrazení zpráv:** ✅ obrázky, ✅ reakce, ✅ emoji jako text, ✅ klikatelné odkazy (i holé
   adresy), ✅ tučně/kurzíva/podtržení/přeškrtnutí, ✅ zmínky, ✅ kód v řádku i bloky kódu,
   ✅ seznamy, nadpisy, tabulky, ✅ citace, přeposlané zprávy a odpovědi, ✅ odpovídání na zprávy
   (tlačítko ↩, Esc zruší), ✅ posílání reakcí (☺ u zprávy, klik na reakci pod zprávou ji
   přidá/odebere), ✅ klik na citaci
   odpovědi skočí na původní zprávu (z cache; když tam není, ukáže oznámení).
6. ✅ **Úpravy a mazání vlastních zpráv:** ✎ (nebo ↑ v prázdném poli) načte text do psacího pole,
   Enter uloží, Esc zruší; citace odpovědi a obrázky zůstanou, ostatní formátování se nahradí
   textem. 🗑 se potvrzuje. Změna se projeví hned, při chybě se vrátí stav ze serveru.
   Ověřeno uživatelem 2026-11-05.
7. ✅ **Přepínání účtů** v okně: klik na profil v hlavičce panelu otevře menu účtů (přepnutí,
   „Přidat účet“ se Zpět, odhlášení s potvrzením, které smaže token i cache účtu). Aktivní je
   vždy jeden účet; naposledy použitý se pamatuje (`settings.toml` → `account`). Při přepnutí se
   zahodí stav i výsledky rozběhnutých úloh předchozího účtu. Odkaz i kód přihlášení jdou
   zkopírovat. ✅ Nepřečtené z neaktivních účtů: jednou za minutu se stáhne jejich seznam
   konverzací (spojení se drží, tokeny se zbytečně neobnovují); počet je v menu u účtu,
   jako odznak u jména a v součtu v liště. ✅ Notifikace z neaktivních účtů (nové zprávy
   v chatech od minulého dotazu, v nadpisu název účtu); klik přepne účet a otevře chat.
8. ✅ **Hledání ve zprávách:** fulltext SQLite FTS5 uvnitř šifrované cache (bez diakritiky,
   předpony slov, úryvky se zvýrazněním), výsledky v postranním panelu pod chaty a lidmi,
   klik otevře zprávu v kontextu a zvýrazní ji. Indexace historie na pozadí: po startu postupně
   stáhne historii všech konverzací (stránky po 200, 1,5 s pauza, max. rok / 5000 zpráv na
   konverzaci) a navazuje, kde skončila. „Načíst starší“ bere nejdřív z cache.
9. ✅ **Nový skupinový chat** (priorita uživatele 2): tlačítko ＋ vedle hledání v záložce Chaty,
   výběr lidí z adresáře (aspoň dva), nepovinný název. Chat je nejdřív jen v okně a na serveru
   vznikne první zprávou (`POST /v1/threads` s `topic`). Ověřeno uživatelem 2026-11-05.

### Fáze 1b: potřebuje odchyt provozu (HAR z web klienta)
1. **Trouter (WebSocket):** okamžité doručování zpráv, „píše…“, přítomnost. Nahradí 5s obnovu.
   ⏸ Odloženo 2026-11-04 do pořízení HAR. 🔬 **HAR 2026-11-11 obsahuje celý protokol**
   (`docs/protocol/README.md` § 4): WebSocket `go-eu.trouter…/v4/c`, přihlášení tokenem
   `ic3.teams.office.com`, rámce socket.io 0.9, registrace adresy v registraru, odběr
   přítomnosti; zprávy chodí jako stejná `EventMessage` jako u long-pollu, takže je
   zpracuje stávající parser. Připraveno k implementaci; registrace se hlásí šablonou
   oficiálního klienta (`TeamsCDLWebWorker_2.7`) → potřeba souhlas uživatele.
2. ✅ **Hlášení přečtení** serveru (consumption horizon), aby se nepřečtené shodovaly s web klientem.
   Hotovo (2026-11-04), ověřeno uživatelem 2026-11-05: otevřený chat se nahlásí jako přečtený, když
   má okno fokus a není skryté (při návratu do okna hned). Přečtení se teď i lokálně počítá
   jen při viditelném okně (dřív se otevřený chat označil jako přečtený i ve skrytém okně).
   Priorita uživatele (2026-11-04): 1. hlášení přečtení, 2. založení skupinového chatu,
   dál podle mých priorit; každý bod samostatný commit, testovat bude uživatel najednou.
3. ✅ **Jména účastníků** chatů bez názvu z profilů (middle tier), místo procházení historie:
   u 1:1 podle id chatu, u skupin podle členů vlákna (ověřeno), pak `fetchShortProfile`;
   i u chatů, které přibudou za běhu. Lidé z jiných organizací profil nevrátí, pro ně
   zůstává záloha z historie zpráv.
4. **Odesílání souborů:** pracovní účet = OneDrive „Microsoft Teams Chat Files“ + `properties.files`
   (OneDrive povolen jen na tohle, 2026-11-09); osobní = AMS upload. Výběr souboru a
   drag & drop do okna, průběh nahrávání. Potřeba HAR s odesláním souboru (chat i kanál).
5. **Osobní účty:** ověřit autoritu, client id, scope, authz a chat host.
6. ✅ **Přihlášení s PKCE** pro organizace, které blokují device code. Uživatel narazil (2026-11-04)
   u druhého pracovního účtu na chybu 53003 (blokováno podmíněným přístupem, zařízení
   „Unregistered“). Pokud politika blokuje jen device code, pomůže přihlášení v prohlížeči;
   pokud vyžaduje spravované zařízení, neprojde žádný neoficiální klient.
   Hotovo (2026-11-04): tlačítko „Přihlásit pracovní účet v prohlížeči“ (a odkaz z obrazovky
   s kódem). Otevře systémový prohlížeč; adresu, na které prohlížeč skončí, Tymy převezme
   ze schránky při návratu do okna, nebo z pole. Návratová adresa jde přepsat v
   `endpoints.toml` (`redirect_uri`, s `http://localhost` bez kopírování). Ověřeno: výchozí je
   `http://localhost` (místní server převezme kód sám), uživatel se tak přihlásil i účtem
   s chybou 53003 u kódu. `…/oauth2/nativeclient` v systémovém prohlížeči nefunguje
   (Microsoft přesměruje na `/common/wrongplace` a kód se ztratí). Přihlášení v prohlížeči
   je teď hlavní tlačítko, kód je druhá možnost.

### Fáze 1c: distribuce
1. ✅ Balíček `.deb`: `packaging/build-deb.sh` (místo `cargo-deb` jen `dpkg-deb` a
   `dpkg-shlibdeps`), `tymy` + `tymy-cli`, `.desktop` soubor, ikony (SVG + PNG 16–256),
   okno má WM_CLASS/app id `tymy` a vlastní ikonu. Závislosti: knihovny z `dpkg-shlibdeps` +
   ručně ty, které winit/wgpu načítají za běhu (X11, xkbcommon, Wayland, font); GPU ovladače
   jen doporučené (bez nich iced kreslí softwarově).
   ✅ Autostart: volba „Spouštět po přihlášení“ v menu účtů zapíše
   `~/.config/autostart/tymy.desktop` (pro jednoho uživatele, balíček nic do `/etc/xdg`
   neinstaluje); po přihlášení se Tymy spustí s `--hidden` jen v liště (bez lišty okno ukáže).
2. ✅ Test instalace na čistém Ubuntu 24.04 (Docker, 2026-11-04): instalace bez chybějících
   knihoven, okno pod Xvfb běží (přihlašovací obrazovka).
3. Volitelně Flatpak později.

### Fáze 2: hovory a videohovory (výzkum s rozhodovacím bodem)
1. Výzkum (časově omezený, cca 2–4 týdny): odchyt signalizace 1:1 hovoru z web klienta.
   Média jsou standardní WebRTC (DTLS-SRTP, Opus, H.264/VP8); neznámá je proprietární signalizace.
   🔬 **První HAR (2026-11-10, testovací hovor s echo botem):** signalizace přes flightproxy
   (`epconv` se SDP nabídkou, `conv/…/add`, `leave`, broker `subscribe`), rozbor
   v `docs/protocol/README.md` § 9. Zjištění: i 1:1 hovor je ad hoc schůzka na mediálním
   serveru Microsoftu; SDP je WebRTC s rozšířeními MS (13 m-řádků v BUNDLE, `x-data` přes
   SCTP, `RTP/SAVP` + DTLS). **Odpověď protistrany (SDP) a události hovoru chodí přes
   trouter**, takže trouter je nutný i pro odchozí hovory → trouter je další krok.
   Záznam neobsahoval WebSocket (bez F5) ani tokeny (sanitized); potřeba nový záznam:
   F5 + testovací hovor, export „with sensitive data“. Echo bot je ideální protistrana
   pro prototyp (nikoho neruší).
   🔬 **Druhý HAR (2026-11-11, s trouterem):** SDP odpověď mediálního serveru přichází
   v události `call/acceptance` (relay kandidáti na portu 3478, `setup:passive`), TURN
   údaje dává `trap/tokens`, průběh hovoru hlásí `conversationUpdate`/`rosterUpdate`/`call/end`.
   Signalizace je tím popsaná celá; neznámá zůstává autorizace (záznam opět sanitized,
   vyzkouší se) a kompatibilita médií: SDP je dialekt Microsoftu (`RTP/SAVP`, `x-data`,
   `x-ssrc-range`), `webrtc-rs` ho bez úprav nevezme.
2. Prototyp v `tymy-cli`: odchozí 1:1 audio hovor; média přes `webrtc-rs`, záloha `libwebrtc`;
   zvuk přes PipeWire (`cpal`).
3. **Rozhodovací bod:**
   - funguje → příchozí hovory (zvonění přes trouter), video (kamera `nokhwa`), ztlumení, výběr zařízení;
   - nefunguje → záloha: tlačítko „Zavolat / Připojit“ otevře hovor v prohlížeči.
4. Skupinové hovory a schůzky až po úspěšném 1:1.

### Fáze 3: kalendář a připomínky
Priorita uživatele 2026-11-09 (předběhla HAR a hovory); čtení bez odchytu provozu přes middle tier.
1. ✅ Načtení událostí (pracovní účet): middle tier Teams
   `v2.1/me/calendars/default/calendarView` se stejným přihlášením jako chat (žádný Graph).
   Osobní účet: neověřeno (Outlook.com).
2. ✅ Záložka **Kalendář** (druhý řádek pod Chaty/Týmy): agenda dnes + 14 dní po dnech,
   hledání v názvu/místě/organizátorovi, detail (čas, místo, organizátor, moje odpověď,
   text pozvánky), „Připojit“ (zatím v prohlížeči, fáze 2) a „Chat schůzky“.
3. 🟡 Připomínky: kontrola každých 30 s, obnova kalendáře po 5 min podle hodin (po probuzení
   ze spánku hned a připomínky počkají na čerstvá data). Notifikace X minut předem
   (`settings.toml` → `[calendar] reminder_minutes`, výchozí 15; `reminders = false` vypne),
   celodenní události v 8:00, zrušené a odmítnuté ne. Připomínky ze všech přihlášených účtů,
   klik otevře událost (případně přepne účet), „Připojit“, „Odložit o 5 min“ (nejdéle do
   začátku). Zobrazené připomínky v `~/.local/state/tymy/calendar-reminders.json`, aby se
   po restartu neopakovaly; spuštění během schůzky ji ještě připomene.
   Ikona v liště: oranžové hodiny + popisek a položka menu „Schůzka: …“, dokud si uživatel
   schůzku neotevře (klik na notifikaci, menu ikony, výběr v agendě, záložka Kalendář,
   „Připojit“) nebo dokud neskončí.
4. 🟡 **Plánování schůzek** (2026-11-09, uživatel dodal HAR vytvoření a smazání schůzky
   z web klienta): „＋“ v záložce Kalendář otevře formulář (název, datum, od–do, účastníci
   z adresáře, Teams schůzka, místo, popis); u vlastní schůzky „Zrušit schůzku…“ (potvrzení),
   u pozvánky Přijmout / Předběžně / Odmítnout. Zápis přes Microsoft Graph (stejné přihlášení,
   `Calendars.ReadWrite`); web klient jde přes Outlook (OWA + GraphQL), viz protokol.
   Formulář ověřen v okně, zápis na server čeká na test. Ladění: `TYMY_OPEN_TAB=meeting`.
   Připomínka uživatele (2026-11-09): „když kliknu přidat schůzku, mělo by zmizet vybrání schůzky
   ve sloupci“ → při otevřeném formuláři není v agendě nic vybrané.
5. ✅ **Týdenní náhled** (2026-11-09, „bylo by možné místo náhledu schůzky vytvořit klasický
   týdenní náhled kalendáře?“): vpravo mřížka Po–Ne × 0–24 h (po otevření posunutá na pracovní
   dobu), přepínání ‹ Dnes ›, celodenní události nahoře, souběžné schůzky vedle sebe, čára
   aktuálního času, předběžné a nezodpovězené s obrysem, zrušené/odmítnuté tlumeně. Klik na
   schůzku (v mřížce i v agendě) otevře detail jako kartu nad mřížkou (✕, Esc, klik vedle).
   Týdny mimo 14 dní se načítají zvlášť. „duplicitní tlačítko nová schůzka. ponechal bych jen
   jedno v pravém rohu“ → jen „＋ Nová schůzka“ vpravo nahoře.
6. 🟡 **Výběr času tažením** (2026-11-09, „přidej ještě možnost označit blok v jednom dni
   v kalendáři a po uvolnění tlačítka myši zobraz dialog nová schůzka“): tažením v prázdném
   místě dne se označí čas (po čtvrthodinách, s popiskem od–do), po puštění tlačítka se otevře
   formulář nové schůzky s datem a časem; klik bez tažení = 30 minut. Výběr zůstává v jednom
   dni, i když myš přejde jinam; puštění se hlídá v celém okně. Ovládání myší ověří uživatel.
7. 🟡 **Plovoucí formulář** (2026-11-09, „dialog nová schůzka přepni do overlay dialogu, který
   půjde drag&drop aby bylo možné jej přemístit nad kalendářem tak aby bylo vidět sloupec
   v kalendáři“): formulář je karta nad týdenní mřížkou bez ztmavení (výchozí vpravo nahoře),
   přesouvá se tažením za hlavičku; zvolený čas je v mřížce zvýrazněný, další tažení v mřížce
   jen změní datum a čas ve formuláři; ✕ nebo Esc zavře. Ovládání myší ověří uživatel.
8. Volitelně barvy kategorií, opakované schůzky, úprava schůzky, přesun schůzky v mřížce.

## 5. Moje doporučení k pořadí
Stav 2026-11-05: fáze 1a a 1c hotové, z 1b vše, co nepotřebuje odchyt provozu
(hlášení přečtení, jména účastníků, přihlášení v prohlížeči, stránkování seznamu).
2026-11-05 navíc: líné načítání starších chatů, vynechání smazaných týmů (`isdeleted`).
1. ✅ **Souhrnný test uživatelem** (2026-11-05: „zatím se zdá, že funguje vše, jak má“):
   reakce, úpravy a mazání, odpovědi, nový 1:1 a skupinový chat, hlášení přečtení,
   skok z citace, odhlášení účtu, notifikace z neaktivního účtu, spouštění po přihlášení,
   instalace `.deb`. Obrázek ze schránky po opravě podle HAR ověřen zvlášť (bod 14).
2. **Společný odchyt provozu (HAR)** z web klienta (Chrome; postup a scénáře
   v `docs/protocol/README.md`, očištění `tools/har-redact.py`), v tomto pořadí důležitosti:
   trouter (WebSocket rámce), odeslání souboru, hledání lidí mimo organizaci,
   případně osobní účet (teams.live.com). Kalendář HAR nepotřeboval (2026-11-09, fáze 3).
3. Podle HAR: **trouter** (okamžité zprávy, „píše…“, přítomnost; předpoklad hovorů),
   pak **soubory** (OneDrive jen pro odeslání).
4. Fáze 2 (hovory) až po trouteru. Fáze 3 (kalendář) předběhla na přání uživatele
   (2026-11-09): endpoint je dostupný se stejným přihlášením, hotovo bez HAR.

8. ✅ **Posouvání vlákna:** vždy na nejnovější zprávu, overlay „Nové zprávy“ při čtení starších.
9. ✅ **Logování:** `--log=debug|production`, `TYMY_LOG`, `settings.toml`; `~/.local/state/tymy/logs`.
10. ✅ **Hledání lidí** v adresáři + otevření/založení 1:1 chatu. Zbývá: lidé mimo organizaci (HAR).
11. ✅ **Záložky Chaty / Týmy**, odznaky nepřečtených, **ruční pořadí** připnutých chatů.
12. ✅ **Design:** vlastní paleta, avatary, přepínač vzhledu (`settings.toml` → `theme`).
13. ✅ **Kopírování:** ⧉ celá zpráva, ⌶/dvojklik režim výběru, automatické kopírování výběru
    (schránka i primární výběr X11).
14. ✅ **Obrázky ze schránky:** Ctrl+V, náhledy s ✕, Backspace ✅; odeslání: podle HAR
    (2026-11-05) přes `eu-prod.asyncgw.teams.microsoft.com`, `sharingMode: Inline`,
    `amsreferences` ve zprávě a hlavička `x-ms-client-version: 1415/…` (verze web klienta;
    uživatel souhlasil 2026-11-05). Nahrání ✅ (`tymy-cli upload`), odeslání z okna ✅
    ověřeno uživatelem (2026-11-05).

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
