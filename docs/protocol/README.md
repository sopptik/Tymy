# Protokol Teams: pracovní poznámky

Tymy používá **interní, nedokumentovaná API** web klienta Teams. Tento dokument je
živý záznam toho, co víme, a hlavně co je potřeba ověřit. Všechny adresy
a identifikátory jsou v `crates/api/src/endpoints.rs` a dají se přepsat bez
rekompilace v `~/.config/tymy/endpoints.toml`:

```toml
[work]
# client_id = "..."
# authz_url = "..."
# redirect_uri = "http://localhost"   # přihlášení v prohlížeči, viz 1.

[personal]
# authority = "https://login.microsoftonline.com/consumers"
# client_id = "..."
# skype_scope = "..."
# authz_url = "..."
# chat_service = "..."
```

Legenda: ✅ ověřeno proti produkci · 🟡 převzato z veřejných zdrojů, neověřeno námi · ❓ hypotéza

## 1. Přihlášení

| Krok | Pracovní účet | Osobní účet |
|---|---|---|
| Autorita | `login.microsoftonline.com/organizations` ✅ | `login.microsoftonline.com/consumers` ❓ |
| Client id | `1fec8e78-bce4-4aaf-ab1b-5451cc387264` (Teams desktop) ✅ | stejné ❓ |
| Flow | device code (RFC 8628) ✅ | device code ❓ |
| Scope | `https://api.spaces.skype.com/.default` ✅ | `https://api.fl.spaces.skype.com/.default` ❓ |

Device code flow může být v organizaci zablokovaný podmíněným přístupem
(chyba `AADSTS50199`/`AADSTS53003` apod.). Pro ten případ je **přihlášení v prohlížeči**
(authorization code + PKCE, `crates/api/src/auth/browser.rs`) 🟡:

- `GET {autorita}/oauth2/v2.0/authorize?client_id&response_type=code&response_mode=query`
  `&redirect_uri&scope&code_challenge&code_challenge_method=S256&state&prompt=select_account`
  v systémovém prohlížeči,
- `POST {autorita}/oauth2/v2.0/token` s `grant_type=authorization_code`, `code`, `redirect_uri`,
  `code_verifier`, `scope`.
- Návratová adresa (`redirect_uri` v `endpoints.toml`): výchozí `http://localhost` ✅
  (port se doplní, kód převezme místní server Tymy). Ověřeno 2026-11-04 s pracovním účtem,
  kterému organizace blokuje device code (`53003`).
- `https://login.microsoftonline.com/common/oauth2/nativeclient` v systémovém prohlížeči
  **nefunguje** ✅: Microsoft adresu s kódem hned přesměruje na `/common/wrongplace`
  („Toto není správná stránka“) a kód se ztratí. Funguje jen ve vestavěném prohlížeči,
  který navigaci zachytí. Jiná (ne-localhost) adresa: Tymy nabídne pole pro vložení adresy.
- Microsoft návratové adresy u `/organizations` i u konkrétního tenantu kontroluje až po
  přihlášení (vymyšlená adresa neskončí chybou `AADSTS50011` hned).
- Politika vyžadující spravované zařízení (`53003` se stavem zařízení „Unregistered“ i v
  prohlížeči) neprojde ani tak.

## 2. Výměna za skypetoken

- Pracovní: `POST https://authsvc.teams.microsoft.com/v1.0/authz`, `Authorization: Bearer <access token>`, prázdné tělo ✅
  - Odpověď: `tokens.skypeToken`, `tokens.expiresIn`, `regionGtms.chatService` (např. `https://emea.ng.msg.teams.microsoft.com`) ✅
- Osobní: `POST https://teams.live.com/api/auth/v1.0/authz/consumer` ❓
- Parser (`auth/skype.rs`) je záměrně tolerantní: token i expiraci hledá podle názvu klíče kdekoli v odpovědi.

## 3. Chat služba (chatsvc)

Hlavička `Authentication: skypetoken=<token>`. Id konverzací se v cestě kódují jako segment.

| Operace | Požadavek | Stav |
|---|---|---|
| Seznam konverzací | `GET /v1/users/ME/conversations?view=msnp24Equivalent&pageSize=200` | ✅ |
| Historie | `GET /v1/users/ME/conversations/{id}/messages?view=msnp24Equivalent\|supportsMessageProperties&pageSize=50&startTime=1`; starší stránky přes `_metadata.backwardLink` | ✅ |
| Odeslání | `POST /v1/users/ME/conversations/{id}/messages`, tělo `{content, messagetype: "RichText/Html", contenttype: "text", clientmessageid, imdisplayname}` | ✅ |

Formát zprávy: `id` (ms timestamp), `clientmessageid`, `content` (HTML), `messagetype`
(`RichText/Html`, `Text`, `ThreadActivity/*`, `Event/*`, `Control/Typing`, …),
`from` (URL končící MRI odesílatele), `imdisplayname`, `composetime`,
`originalarrivaltime`, `properties` (`edittime`, `deletetime`, `files` jako JSON řetězec,
`emotions` = reakce).

| Operace | Požadavek | Stav |
|---|---|---|
| Historie po 200 | `pageSize=200` funguje (víc server odmítne); i v `backwardLink` | ✅ |
| Seznam konverzací po stránkách | `_metadata: {backwardLink, forwardLink, syncState, totalCount}`; `backwardLink` míří na `/v1/users/8:orgid:<oid>/conversations?…&syncState=…` a vede na starší konverzace, poslední stránka ho nemá (ověřeno po 10: 4 stránky = všech 39) | ✅ |
| Reakce přidat | `PUT …/messages/{id}/properties?name=emotions`, tělo `{"emotions":{"key":"like","value":<ms>}}` | ✅ (uživatel 2026-11-05) |
| Reakce odebrat | `DELETE …/messages/{id}/properties?name=emotions`, tělo `{"emotions":{"key":"like"}}` | ✅ (uživatel 2026-11-05) |
| Úprava | `PUT …/messages/{id}`, tělo `{id, type:"Message", conversationid, content, messagetype, contenttype, imdisplayname}` | ✅ (uživatel 2026-11-05) |
| Smazání | `DELETE …/messages/{id}?behavior=softDelete` | ✅ (uživatel 2026-11-05) |
| Přečteno | `PUT …/conversations/{id}/properties?name=consumptionhorizon`, tělo `{"consumptionhorizon":"<id zprávy>;<čas ms>;<clientmessageid>"}` (formát jako v seznamu konverzací) | ✅ (uživatel 2026-11-05) |

Přečtení Tymy hlásí jen pro otevřený chat, když má okno fokus a není skryté; jednou pro
každou novou zprávu (i po chybě až s další zprávou).

### Zmínky při odesílání 🟡

Obsah: `<span itemtype="http://schema.skype.com/Mention" itemscope="" itemid="0">Jan Novák</span>`
(bez „@“), `itemid` = pořadí zmínky ve zprávě. V těle zprávy `"properties": {"mentions":
"[{\"@type\":\"http://schema.skype.com/Mention\",\"itemid\":0,\"mri\":\"8:orgid:<oid>\",
\"mentionType\":\"person\",\"displayName\":\"Jan Novák\"}]"}` (seznam jako JSON řetězec).
Tvar podle přijatých zpráv a známých klientů; odeslání čeká na ověření (2026-11-09).

### Obsah zpráv (HTML) ✅

Ověřeno na ~1200 reálných zprávách (struktura značek, bez obsahu):

| Prvek | HTML |
|---|---|
| Odstavec, prázdný řádek | `<p>…</p>`, `<p>&nbsp;</p>`; mezi bloky bývá `\r\n` (nevýznamné) |
| Formátování | `<strong>`/`<b>`, `<i>`, `<u>`, `<s>`, `<code>`, `<h1>`–`<h3>`, `<ul>/<ol>/<li>`, `<hr>`, `<table>` |
| Odkaz | `<a href=… rel="noreferrer noopener" target="_blank" title=…>`; soubory `itemtype="filesHyperlink"` nebo `…/HyperLink/Files` |
| Zmínka | `<span itemtype="http://schema.skype.com/Mention" itemscope itemid="0">Jméno</span>`; `itemid` = index do `properties.mentions` (`mri`, `mentionType` person/channel, `displayName`) |
| Emoji | `<span class="animated-emoticon-20-smile" type="(smile)"><img itemtype="http://schema.skype.com/Emoji" alt="🙂" …></span>` |
| Blok kódu | `<p itemtype="http://schema.skype.com/CodeBlockEditor">` (popisek, nezobrazovat) a `<pre class="language-php …"><code>` s `<span class="hljs-…">` a `<br>` |
| Odpověď | `<blockquote itemscope itemtype="http://schema.skype.com/Reply" itemid="{id zprávy}"><strong itemprop="mri" itemid="{MRI autora}">Jméno</strong><span itemprop="time" itemid="{id}"></span><p itemprop="preview">zkrácený text</p></blockquote>` a za ním vlastní text; případně `<span itemprop="emojis">` |
| Přeposlání | `<blockquote itemtype="http://schema.skype.com/Forward">…</blockquote>` |

Odeslání odpovědi: tentýž `blockquote` před `<p>` s textem (náhled max. 150 znaků).
**Neověřeno** proti produkci, zda server nevyžaduje ještě něco v `properties`.

## 4. Real-time

Implementováno, ale **nefunguje** (❌, ověřeno 2026-11-04): long-poll zděděný ze Skype protokolu.
`POST /v1/users/ME/endpoints` vrací `410 Gone`, Microsoft ho vypnul. Okno proto zatím
obnovuje seznam konverzací každých 5 s. Původní postup:
1. `POST /v1/users/ME/endpoints` `{endpointFeatures}` → hlavička `Set-RegistrationToken`
2. `POST /v1/users/ME/endpoints/SELF/subscriptions` `{channelType: "httpLongPoll", template: "raw", interestedResources}`
3. opakovaně `POST /v1/users/ME/endpoints/SELF/subscriptions/0/poll` → `{eventMessages: [{resourceType, resourceLink, resource}]}`

Současný web klient používá WebSocket **trouter** (`*.trouter.teams.microsoft.com`).
Pokud long-poll nebude fungovat, je trouter další krok (stejně ho budeme potřebovat
pro příchozí hovory ve fázi 2).

### Reakce ✅

`properties.emotions` (pole, případně JSON řetězec s polem):

```json
[{"key": "like", "users": [{"mri": "8:orgid:<oid>", "time": 1793780837849, "value": "1793780837690"}]}]
```

Klíče: `like`, `heart`, `laugh`, `surprised`, `sad`, `angry` a nově kódy emoji ve tvaru
`<hex>[-<hex>]_<název>` (např. `1f44d_thumbsup`). Položky s prázdným `users` ignorujeme.
Další pozorované klíče `properties` (zatím nepoužité): `deltaEmotions`, `mentions`, `links`,
`qtdMsgs` (citované zprávy), `cards`, `pinned`, `originalMessageContext`.

Ladění: `tymy-cli raw "/v1/users/ME/conversations/<id>/messages?view=msnp24Equivalent|supportsMessageProperties&pageSize=50&startTime=1"`.

## 5. Média ve zprávách (AMS)

Obrázky vložené do zprávy nejsou přílohy (`properties.files`), ale `<img>` značky
přímo v HTML obsahu zprávy:

```html
<img src="https://eu-api.asm.skype.com/v1/objects/0-weu-d5-<hex>/views/imgo"
     itemtype="http://schema.skype.com/AMSImage" itemscope="png"
     width="331.7" height="250" alt="obrázek" itemid="0-weu-d5-<hex>">
```

- Emoji mají `itemtype="http://schema.skype.com/Emoji"`, `src` na `statics.teams.cdn.office.net`
  a znak v `alt`. Zobrazujeme je jako text (`alt`), nestahují se.
- Stažení: `GET <src>` s hlavičkou `Authorization: skype_token <skypetoken>` ✅.
  Pozor, formát se liší od chatsvc (`Authentication: skypetoken=…`).
  Token posíláme jen na `https://*.asm.skype.com` (`crates/api/src/media.rs`).
- Varianty objektu (`/v1/objects/{id}/views/{view}`), ověřeno 2026-11-04:

  | View | Obsah |
  |---|---|
  | `imgo` | zmenšený JPEG náhled (např. 800×603); na něj odkazuje zpráva ✅ |
  | `imgpsh_fullsize` | originál (např. PNG 1088×820) ✅ |
  | `imgpsh_fullsize_anim` | originál, u GIFů s animací; používáme po kliknutí ✅ |
  | `original` | `400 Bad Request` ❌ |

- Nahrání obrázku (odeslání ze schránky) podle HAR web klienta (2026-11-05):
  1. `POST https://eu-prod.asyncgw.teams.microsoft.com/v1/objects/` (brána `asyncgw`, ne
     `eu-api.asm.skype.com`), hlavičky `x-ams-post-sharing-mode: Inline`,
     `x-ms-client-version: 1415/<verze>`, přihlášení v HAR chybí (export „sanitized“ odstranil
     `Authorization`), tělo `{"type": "pish/image", "permissions": {"<id chatu>": ["read"]},
     "sharingMode": "Inline", "filename": "…"}` → `201 {"id": "0-frca-d5-…"}`, `Location`;
  2. `PUT …/v1/objects/{id}/content/imgpsh`, `application/octet-stream` → `201`;
  3. zpráva `RichText/Html` s `<img src="https://eu-api.asm.skype.com/v1/objects/{id}/views/imgo"
     itemtype="http://schema.skype.com/AMSImage" itemscope="png" width height id itemid …>`
     (adresa na starém hostu) a v těle `"amsreferences": ["{id}"]`, v `properties` `blurHash`;
  4. web klient pak čte náhled z `…asyncgw…/v1/{oid uživatele}/objects/{id}/views/imgo?v=1`.
  Bez `x-ms-client-version` ❌ `400 {"title":"The 'User-Agent' header is missing or invalid.",
  "detail":"The platform id could not be created from the '…' value of 'User-Agent' header."}`
  na obou hostech, s `User-Agent` Tymy i Chrome (chyba mluví o User-Agent zavádějícím způsobem).
  S `x-ms-client-version: 1415/26101515216` (verze web klienta, souhlas uživatele 2026-11-05)
  a vlastním `User-Agent` ✅ `201`. Verze je v `endpoints.toml` → `media_client_version`.
  Ladění: `tymy-cli upload <chat> <soubor>` (nahraje, zprávu neodešle).
  Odeslání zprávy s obrázkem z okna ✅ ověřeno uživatelem (pracovní účet, 2026-11-05).
- „Píše…“ (odchozí): web klient posílá `POST …/messages` s `{"content": "", "contenttype":
  "Application/Message", "messagetype": "Control/Typing"}` → `201` ✅ (HAR 2026-11-05).
- Hlavičky web klienta k chat službě: `clientinfo: os=linux; …; clientName=skypeteams;
  clientVer=1415/…`, `behavioroverride: redirectAs404`, `x-ms-migration: True`; chat služba
  běží i přes proxy `https://teams.cloud.microsoft/api/chatsvc/emea/v1/…`.
- Metadata objektu: `GET /v1/objects/{id}` vrací JSON, např. `{"type": "pish/image",
  "filename": "MicrosoftTeams-image.png", …}` ✅.
- Ladění: `tymy-cli media <url> -o soubor`.

## 6. Middle tier (profily, hledání lidí)

Adresa z authz odpovědi: `regionGtms.middleTier` (např. `https://teams.microsoft.com/api/mt/emea`).
Autorizace: `Authorization: Bearer <access token pro skype_scope>` ✅ (stejný token, který se
vyměňuje za skypetoken); posíláme i `X-Skypetoken`. Ladění: `tymy-cli mt <METODA> <cesta> [json]`.

| Operace | Požadavek | Stav |
|---|---|---|
| Profily podle MRI | `POST beta/users/fetchShortProfile?isMailAddress=false&enableGuest=true&includeIBBarredUsers=true&skypeTeamsInfo=true`, tělo `["8:orgid:<oid>", …]` → `{value: [{mri, displayName, email, jobTitle, department, …}]}` | ✅ |
| Hledání v adresáři | `POST beta/users/searchV2?includeDLs=false&includeBots=false&enableGuest=true&source=newChat&skypeTeamsInfo=true`, tělo = JSON řetězec dotazu → `{value: [...], bestMatchSuggestions: [...]}` | ✅ |
| Hledání mimo organizaci | `GET beta/users/<email>/externalsearchv3?includeTFLUsers=true` → interně `userdiscovery.teams.microsoft.com/v4/discovery/…` | ❌ `400 BadRequest` (chybí hlavička/token, potřeba HAR) |
| Kalendář | `GET v2.1/me/calendars/default/calendarView?StartDate=2026-11-09T00:00:00.000Z&EndDate=…&shouldDecryptData=true` → `{count, value: [událost…]}` | ✅ 2026-11-09, oba pracovní účty |

Plánování schůzek (HAR web klienta 2026-11-09, vytvoření a zrušení schůzky; export
„sanitized“, tedy bez hlaviček `Authorization`): kalendář web klienta je vložený Outlook
(`x-owa-host-app: teams_desktop`, `x-owa-hosted-ux: true`).
1. `POST outlook.office.com/outlookgatewayb2/graphql` mutace `createOnlineMeeting`
   (`mailboxId` = e-mail, `calendarId` = id složky kalendáře, `providerType: TeamsForBusiness`,
   `providerAdditionalProperties: [{key: "meetingType", value: "Scheduled"}]`) → `joinUrl`
   a `providerProperties` (`links.options`, `links.update`, `links.shortJoinUrl`);
2. `POST outlook.office.com/owa/service.svc?action=CreateCalendarEvent&app=Calendar`
   (hlavička `action`), tělo `CreateItemJsonRequest:#Exchange` (`RequestServerVersion:
   V2018_01_08`, `TimeZoneContext` „Central Europe Standard Time“), položka
   `CalendarItem:#Exchange`: `Subject`, `Start`/`End` místní čas + `StartTimeZoneId`,
   `RequiredAttendees[].Mailbox{EmailAddress, Name, MailboxType, RoutingType: SMTP,
   AADObjectId}`, `IsOnlineMeeting`, `OnlineMeeting{joinUrl, providerProperties}`,
   `OnlineMeetingProvider: TeamsForBusiness`, `ExtendedProperty` (sada
   `00020329-0000-0000-C000-000000000046`, `SchedulingServiceUpdateUrl`,
   `SchedulingServiceMeetingOptionsUrl`), `ReminderMinutesBeforeStart: 15`,
   `ItemId`/`ClientSeriesId` = `localevent:<uuid>` → `Success`/`NoError`, `ItemId` události;
3. zrušení: `…/service.svc?action=CancelCalendarEvent`, tělo v hlavičce `x-owa-urlpostdata`
   (`CancelCalendarEventJsonRequest`: `EventId`, `EventScope: 1`) → `NoError`.

Tymy místo toho používá **Microsoft Graph** (zdokumentovaný, jedno volání): stejné
přihlášení (Teams desktop client id) dostane token pro `https://graph.microsoft.com/.default`
s `Calendars.ReadWrite` ✅ a pro `https://outlook.office.com/.default` s `Calendars.ReadWrite`,
`OWA.AccessAsUser.All` ✅ (2026-11-09, `tymy-cli token <scope>` vypíše jen nároky). Id událostí
z middle tieru (`objectId`) Graph zná přímo ✅ (`tymy-cli calendar --check-graph`, jen čtení).
- vytvoření: `POST /v1.0/me/events` `{subject, start/end {dateTime, timeZone: UTC}, attendees
  [{type: required, emailAddress}], isOnlineMeeting: true, onlineMeetingProvider:
  teamsForBusiness, isReminderOn, reminderMinutesBeforeStart, body, location}`;
- zrušení (organizátor): `POST /v1.0/me/events/{id}/cancel` `{comment}`;
- odpověď: `POST /v1.0/me/events/{id}/accept|tentativelyAccept|decline` `{sendResponse: true}`.
Zápis zatím neověřen (čeká na souhlas/test uživatele). Ladění: `tymy-cli graph <cesta>` (jen GET,
vypíše tvar odpovědi bez hodnot).

Kalendář (`tymy-cli calendar`): opakované schůzky přijdou jako jednotlivé výskyty
(`eventType: Occurrence`, `seriesMasterId`), časy `startTime`/`endTime` v UTC s posunem
(`2026-11-09T09:00:00+00:00`), `eventTimeZone` je název pásma Windows. Použitá pole:
`objectId` (id výskytu), `subject`, `isAllDayEvent` (začátek/konec = půlnoci v pásmu události),
`location`, `organizerName`, `bodyPreview`, `isCancelled`, `isOrganizer`, `myResponseType`
(`Accepted`, `NotResponded`, …), `showAs` (`Busy`, `Tentative`, …), `skypeTeamsMeetingUrl`
(odkaz „Připojit“), `skypeTeamsDataObject.cid` (chat schůzky `19:meeting_…@thread.v2`).
Odpověď **neobsahuje** čas připomenutí (`reminderMinutesBeforeStart` čteme, pokud by přišel);
výchozí je proto z `settings.toml`. Další pole: `iCalUID`, `cleanGlobalObjectId`,
`isPrivate`, `isResponseRequested`, `hasAttachments`, `categories`, `lastModifiedTime`.

## 7. Týmy a kanály ✅

Prázdné konverzace (ještě bez jediné zprávy) mají v `properties` `isemptyconversation: "True"`
a žádnou `lastMessage`: chaty naplánovaných schůzek, 1:1 s aplikací (`28:…`), nové kanály.
Web klient je v seznamu chatů neukazuje; Tymy také ne (kromě rozepsaného nového chatu
a otevřeného chatu), kanály ano (2026-11-09).

Kanály jsou v seznamu konverzací (`@thread.tacv2`). `threadProperties`:
- kořen týmu (zároveň obecný kanál): `threadType: "space"`, `productThreadType: "TeamsTeam"`,
  `spaceThreadTopic` = název týmu, `groupId`,
- kanál: `threadType: "topic"`, `productThreadType: "TeamsStandardChannel"`, `topic` = název kanálu,
  `spaceId` = id kořenového vlákna týmu, `groupId`.
- `lastjoinat` / `lastleaveat` (ms, text nebo číslo) ✅: u opuštěných týmů je odchod pozdější
  než připojení. Taková vlákna seznam konverzací vrací dál, oficiální klient je nezobrazuje
  a zprávy často končí `403` s `{"errorCode":209, "subCode":"AclCheckFailed", "details":
  "The initiator … is not a member of the roster …"}`. Tymy je ze seznamu vynechává
  (skupinové chaty ne); na `403` u jiných konverzací je skryje za běhu.
- `isdeleted: true` (bool) a `deleteTime` ✅ (2026-11-05): smazaný (zrušený) tým. Seznam
  konverzací ho vrací dál, historie odpoví `404` `{"subCode":"ThreadNotFound", "details":
  "Thread was not found as it is hard-deleted."}`. Tymy ho vynechává, i s kanály, jejichž
  `spaceId` míří na smazaný tým (samy příznak nemají).

## 8. Nový 1:1 chat

- Id 1:1 chatu: `19:<oid1>_<oid2>@unq.gbl.spaces`, object id seřazená vzestupně (pozorováno na všech
  existujících chatech) ✅.
- Dokud chat neexistuje, `GET …/conversations/<id>/messages` vrací `404` se `subCode: LocationLookupFailed` ✅.
- Založení: zkusíme odeslat zprávu na odvozené id; při 404 `POST /v1/threads`
  `{members: [{id, role: "Admin"}…], properties: {threadType: "chat", fixedRoster: "true", uniquerosterthread: "true"}}`,
  id z hlavičky `Location` ✅ (uživatel 2026-11-05).
- Skupinový chat: stejné `POST /v1/threads` se všemi členy, `fixedRoster: "false"`,
  `uniquerosterthread: "false"` a volitelně `topic` v `properties` ✅ (uživatel 2026-11-05). Tymy ho
  zakládá až s první zprávou, do té doby je jen v okně (dočasné id `draft:group:<n>`).
- Správa skupinového chatu (2026-11-09, neověřeno, bez HAR): přejmenování
  `PUT /v1/threads/{id}/properties?name=topic` `{"topic": "…"}`, přidání člena
  `PUT /v1/threads/{id}/members/{mri}` `{"role": "Admin"}`, odebrání a opuštění (vlastní MRI)
  `DELETE /v1/threads/{id}/members/{mri}`.
- Členové chatu: `GET /v1/threads/{id}?view=msnp24Equivalent` → `{id, members: [{id: "8:orgid:…",
  role, …}], properties, …}` ✅ (2026-11-04, skupinový chat, 12 členů).
- Jména pro seznam chatů bez názvu: 1:1 = druhé object id přímo z id chatu, skupina = členové
  vlákna; obojí pak `fetchShortProfile` (viz 6.). Pro lidi z jiných organizací vrací
  `fetchShortProfile` prázdné `value` ✅; pak záloha z odesílatelů v historii.

## 9. Hovory (signalizace) 🔬

Zdroj: HAR testovacího hovoru z web klienta (2026-11-10, „Uskutečnit testovací hovor“, echo bot).
Záznam byl pořízen bez obnovení stránky a jako „sanitized“, takže v něm **chybí WebSocket
trouteru** (0 rámců) a tokeny (nevíme, jaký `aud` flightproxy chce).

Všechna volání jdou přes **flightproxy** (`api[-emea].flightproxy.teams.microsoft.com/api/v2/…`),
které předává požadavky interním službám Skype (`conv`, `cc`, `broker`):

| Krok | Volání | Poznámka |
|---|---|---|
| založení | `POST api-emea.flightproxy…/api/v2/epconv` → 201 | tělo ~40 KB, viz níže; odpověď: `conversationController`, `links` (leave, addParticipant, mute, unmute, subscribe, …), `state` |
| odběr událostí | `GET …/ep/broker-…/api/v1/subscribe/{uuid}/0` | long-poll po 30 s (`nextSubscribeUrl`, `expirationTimeInSec`), hlavička `x-usebatching: 1`; v záznamu prázdný |
| pozvání protistrany | `POST …/conv/{id}/add` → 202 | `participants.from` (já: `8:orgid:…`, `endpointId`, `participantId`, `languageId`), `to` (MRI protistrany), `groupChat.threadId` (1:1 chat), `links.addParticipantSuccess/Failure` |
| metadata | `PUT …/conv/{id}/updateEndpointMetadata` → 200 | odpověď obsahuje `activeModalities.call.links.participants` (call hub `cc`) |
| parametry médií | `POST …/cc/v1/mcProxy/…/applyChannelParameters` → 202 | limity videa (rozlišení, fps, počet streamů) |
| zavěšení | `POST …/conv/{id}/leave` → 204 | `conversationTransactionEnd`, `callTransactionEnd` (`CallEndReasonLocalUserInitiated`) |

Hlavičky: `x-microsoft-skype-client: SkypeSpaces/1415/<verze>/os=linux; …`,
`x-microsoft-skype-chain-id` (UUID na celý hovor), `x-microsoft-skype-message-id` (UUID na
požadavek), `ms-teams-region/partition/ring`, `x-ms-migration: True`.

Tělo `epconv`:
- `conversationRequest`: `roster.rosterUpdate` a `links.*` (conversationEnd, conversationUpdate,
  localParticipantUpdate, addParticipantSuccess/Failure, addModalitySuccess/Failure,
  confirmUnmute, receiveMessage) jsou **URL na trouter klienta**
  (`https://pub-ent-….trouter.teams.microsoft.com:3443/v4/f/{trouter id}/callAgent/{uuid}/{náhodné}/…`).
  Server na ně posílá události, klient je dostane WebSocketem trouteru.
- `scenario: "UserInitiatedTestCall"` u testovacího hovoru.
- `participants.from`, `endpointCapabilities`, `clientEndpointCapabilities`, `endpointState`.
- `callInvitation.callModalities` (3 modality), `callInvitation.links` (progress,
  **mediaAnswer**, acceptance, redirection, end: opět trouter URL) a
  `callInvitation.mediaContent`:
  - `contentType: application/sdp-ngc-1.0`, `blob` = **SDP nabídka** (~34 KB),
    `mediaDescriptions`, `applyChannelParameters`, `mediaLegId`.

SDP nabídka je WebRTC s rozšířeními Microsoftu: `a=group:BUNDLE` přes 13 m-řádků
(1× audio `main-audio`, 11× video `main-video`/`applicationsharing-video`, 1× `m=x-data`
přes SCTP), `RTP/SAVP` s `a=fingerprint` + `a=setup:actpass` (DTLS-SRTP), `a=ice-options:trickle`,
`a=rtcp-mux`, vlastní `a=x-ssrc-range`, `a=x-signaling-fb`. Kodeky: opus, G722, PCMU/PCMA,
RED, CN, telephone-event; video H264, AV1, rtx. V nabídce je jediný host kandidát (trickle),
další kandidáti ani konfigurace relay (TURN) v HTTP provozu nejsou.

Zjištění:
- I 1:1 (testovací) hovor je na serveru **ad hoc schůzka** (`conversationType: adHocMeeting`,
  `isMultiParty`, `isHostless`): média jdou na mediální server Microsoftu (`cc`, `mcProxy`),
  ne přímo mezi klienty.
- Odpověď protistrany (SDP `mediaAnswer`), průběh a konec hovoru chodí **přes trouter**.
  Bez trouteru tedy nejde ani odchozí hovor, nejen příchozí.
- Klient používá upravené WebRTC ve web workeru (`precompiled-webrtc-web-worker`) a WASM
  zpracování zvuku (`wasmvqe`).

Chybí (další záznam): WebSocket trouteru včetně rámců s `mediaAnswer` a dalšími událostmi
hovoru, odkud klient bere relay/TURN servery a jak posílá trickle ICE kandidáty, a `aud`
tokenu pro flightproxy (export „with sensitive data“).

## Jak odchytit provoz (HAR)

Záznam pořizuje uživatel v **Google Chrome** (Chromium, Edge): jen ty ukládají do HAR i rámce
WebSocketu (`_webSocketMessages`), Firefox je vynechává. HAR obsahuje tokeny a cookies, proto
**nikdy do repozitáře** (`*.har` je v `.gitignore`) a analyzuje se jen očištěná kopie.

1. Otevřít `https://teams.microsoft.com/v2/` a přihlásit se (osobní účet: `teams.live.com`).
2. F12 → **Network**: zapnout *Preserve log* a *Disable cache*, filtr nechat **All**
   (WS i Fetch/XHR). Ikonou ⊘ smazat dosavadní záznam.
3. Obnovit stránku (F5), aby se zachytilo připojení trouteru od začátku. Pak provést
   scénáře, mezi nimi pár sekund pauza:
   - **trouter:** nechat Teams 1–2 min v klidu; z jiného zařízení (mobil, Tymy s druhým
     účtem) poslat do chatu zprávu, začít psát („píše…“), přidat reakci; změnit vlastní stav
     (dostupný → nerušit → dostupný);
   - **obrázek:** do chatu vložit obrázek ze schránky (Ctrl+V) a odeslat (✅ zachyceno 2026-11-05, není třeba opakovat);
   - **soubor:** v chatu 📎 → nahrát malý soubor z počítače a odeslat; v kanálu totéž;
   - **hovor:** ⚙ Nastavení → Zařízení → *Uskutečnit testovací hovor* (echo bot, nikoho
     neruší), nechat ho doběhnout nebo po ~20 s zavěsit. Záznam musí začít obnovením
     stránky (F5), jinak v něm chybí trouter, přes který chodí odpověď a události hovoru;
   - **lidé mimo organizaci:** do hledání napsat celou e-mailovou adresu člověka z jiné
     organizace a otevřít s ním chat (nic neposílat);
   - ~~kalendář~~ hotovo: čtení přes middle tier, vytvoření/zrušení schůzky zachyceno
     (HAR 2026-11-09), zápis přes Graph.
4. Network → ⤓ → *Export HAR (with sensitive data)* → uložit **mimo repozitář**, např.
   `~/teams.har`. (Volbu je třeba povolit: DevTools ⚙ → Preferences → Network → *Allow to
   generate HAR with sensitive data*.) Očištěný HAR by vynechal hlavičky `Authorization`
   a cookies; my potřebujeme vědět, který token se kam posílá: skript z tokenu ponechá jen
   `aud`, `scp` a `appid`, zbytek včetně podpisu zahodí.
5. Očistit: `python3 -I tools/har-redact.py ~/teams.har` → `~/teams.redacted.har` + přehled
   volání a rámců trouteru. Skript odstraní tokeny (hlavičky, URL, těla, rámce WebSocketu),
   cookies, e-maily, texty zpráv a jména (`--keep-text` je ponechá) a binární obsah.
   Původní `~/teams.har` pak smazat.
6. Analyzuje se `~/teams.redacted.har`; zjištění se zapíšou sem, fixtury do
   `crates/api/tests/fixtures/` jen syntetické.

Zajímá nás hlavně:
- trouter: `GET …/v4/a` (nebo `/v3/a`) s parametry, odpověď (`socketio`, `surl`, `ccid`),
  registrace `POST https://teams.microsoft.com/registrar/prod/V2/registrations`, URL WebSocketu
  a sled rámců (handshake, autentizace, `ping`, příchozí `3:::{…}` a odpovědi na ně);
- obrázek: host a hlavičky nahrání (`POST …/v1/objects`, `User-Agent`, `x-ms-*`), tělo
  a následné `PUT` s obsahem;
- soubor: nahrání do OneDrive/SharePointu (hostitel, token pro koho), sdílení a tělo odeslané
  zprávy s `properties.files`;
- hledání mimo organizaci: hlavičky a token požadavku na `externalsearchv3` / `userdiscovery`.

Ladicí výstup Tymy: `tymy-cli -vv chats` vypíše těla odpovědí se zredigovanými tokeny.

## Otevřené body

- [x] Ověřit přihlášení, seznam chatů a historii pro pracovní účet (2026-11-04)
- [x] Ověřit odeslání zprávy (z okna, 2026-11-04)
- [ ] Zjistit skutečné hodnoty pro osobní účty (autorita, client id, scope, authz, chat host)
- [x] Ověřit long-poll: nefunguje (410 Gone)
- [ ] Implementovat trouter (WebSocket) pro okamžité doručování
- [x] Hlásit přečtení serveru (consumption horizon); ✅ (uživatel 2026-11-05)
- [x] Jména účastníků chatů bez nutnosti číst historii (profily z middle tier, 2026-11-04)
- [x] Authorization code + PKCE pro organizace s blokovaným device code (2026-11-04)
- [x] Stránkování seznamu konverzací (`_metadata.backwardLink`) pro účty s >200 chaty (2026-11-04)
- [x] Líné načítání starších stránek seznamu (kurzor `backwardLink` z předchozí části) (2026-11-05)
- [x] Zobrazení vložených obrázků (AMS náhled + originál)
- [x] Odesílání obrázků ze schránky (AMS přes `asyncgw`, `amsreferences`) (2026-11-05)
- [ ] Odesílání souborů: work = OneDrive „Microsoft Teams Chat Files“ + `properties.files`
  (OneDrive povolen jen pro odeslání souboru, 2026-11-09); personal = AMS
