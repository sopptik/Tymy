//! Signalizace hovorů (`docs/protocol/README.md` § 9): zkouška spojení s echo botem Teams
//! („testovací hovor“: založení, přijetí serverem, ukončení) a příchozí hovory (zvonění,
//! odmítnutí). Média (zvuk, obraz) se nepřenáší: nabídka SDP je syntetická a nikam nevede.

use rand::RngExt;
use serde_json::{Value, json};
use tymy_core::{Error, Result, UserId};
use url::Url;

use crate::auth::trusted;

/// Echo bot Teams („Uskutečnit testovací hovor“), stejný ve všech organizacích.
pub const ECHO_BOT: &str = "28:cf28171e-fcfd-47e4-a1d6-79460b0b3ca0";
/// Výchozí služba pro založení hovoru (Evropa), když ji authz neuvede.
pub const DEFAULT_CALL_SERVICE: &str =
    "https://api-emea.flightproxy.teams.microsoft.com/api/v2/epconv";

/// Relay (TURN) servery Teams pro média; přihlašovací údaje dává `trap/tokens`.
pub const RELAY_HOSTS: &[&str] = &[
    "euaz.relay.teams.microsoft.com",
    "default.relay.teams.microsoft.com",
];
pub const TRAP_TOKENS: &str = "https://teams.microsoft.com/trap/tokens";

/// Přihlašovací údaje k relay serverům (platí `expires` sekund, typicky týden).
#[derive(Clone, PartialEq, Eq)]
pub struct TurnCredentials {
    pub username: String,
    pub password: String,
}

impl std::fmt::Debug for TurnCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TurnCredentials(<skryto>)")
    }
}

impl TurnCredentials {
    /// Adresy `turn://` pro `webrtcbin` (UDP i TCP na každém relay serveru).
    pub fn urls(&self) -> Vec<String> {
        let enc = |s: &str| url::form_urlencoded::byte_serialize(s.as_bytes()).collect::<String>();
        let (u, p) = (enc(&self.username), enc(&self.password));
        RELAY_HOSTS
            .iter()
            .flat_map(|h| {
                [
                    format!("turn://{u}:{p}@{h}:3478?transport=udp"),
                    format!("turn://{u}:{p}@{h}:443?transport=tcp"),
                ]
            })
            .collect()
    }
}

/// Rozebere odpověď `trap/tokens`: `{"tokens": [{"realm", "username", "password"}], "expires"}`.
pub fn parse_trap_tokens(v: &Value) -> Option<TurnCredentials> {
    let t = v.pointer("/tokens/0")?;
    Some(TurnCredentials {
        username: t.get("username")?.as_str()?.to_owned(),
        password: t.get("password")?.as_str()?.to_owned(),
    })
}

/// Historie hovorů: zprávy konverzace `48:calllogs` s vlastností `call-log` (JSON řetězec).
/// `me` = vlastní MRI (protistrana je ten druhý z volajícího a volaného).
pub fn parse_call_logs(v: &Value, me: &str) -> Vec<tymy_core::call_log::CallRecord> {
    let time = |c: &Value, k: &str| {
        c.get(k)
            .and_then(Value::as_str)
            .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
            .map(|t| t.with_timezone(&chrono::Utc))
    };
    v.get("messages")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|m| {
            let raw = m.pointer("/properties/call-log")?;
            let c: Value = match raw {
                Value::String(s) => serde_json::from_str(s).ok()?,
                v => v.clone(),
            };
            let incoming = c.get("callDirection").and_then(Value::as_str) == Some("incoming");
            let (orig, target) = (
                c.get("originator").and_then(Value::as_str).unwrap_or(""),
                c.get("target").and_then(Value::as_str).unwrap_or(""),
            );
            let (peer, peer_part) = if incoming || target == me {
                (orig, "/originatorParticipant/displayName")
            } else {
                (target, "/targetParticipant/displayName")
            };
            Some(tymy_core::call_log::CallRecord {
                call_id: c.get("callId")?.as_str()?.to_owned(),
                peer: tymy_core::UserId(peer.to_owned()),
                peer_name: c
                    .pointer(peer_part)
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                incoming,
                state: c
                    .get("callState")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned(),
                call_type: c
                    .get("callType")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned(),
                started_at: time(&c, "startTime")?,
                connected_at: time(&c, "connectTime"),
                ended_at: time(&c, "endTime"),
            })
        })
        .collect()
}

/// Počet účastníků z `conversation/rosterUpdate` (`participantCounts.totalParticipants`).
pub fn participant_count(v: &Value) -> Option<u64> {
    v.pointer("/participantCounts/totalParticipants")?.as_u64()
}

/// Výsledek zkoušky signalizace hovoru.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProbeReport {
    /// Popis jednotlivých kroků (bez adres, tokenů a klíčů).
    pub steps: Vec<String>,
    /// Server hovor přijal a poslal odpověď SDP.
    pub accepted: bool,
}

/// Kdo volá.
#[derive(Debug, Clone)]
pub struct Caller {
    pub mri: UserId,
    pub display_name: String,
    /// Id koncového bodu (epid trouteru, na který chodí události hovoru).
    pub endpoint_id: String,
    pub participant_id: String,
}

impl Caller {
    fn json(&self) -> Value {
        json!({
            "id": self.mri.as_str(),
            "displayName": self.display_name,
            "endpointId": self.endpoint_id,
            "participantId": self.participant_id,
            "languageId": "cs-cz",
        })
    }
}

/// Adresy na trouteru klienta, kam server posílá události jednoho hovoru.
#[derive(Debug, Clone)]
pub struct Callbacks {
    /// `<surl>callAgent/<uuid>/`
    base: String,
    id: String,
}

impl Callbacks {
    pub fn new(surl: &str) -> Self {
        let id = uuid::Uuid::new_v4().to_string();
        Self {
            base: format!("{surl}callAgent/{id}/"),
            id,
        }
    }

    /// Adresy se známým `<uuid>` (další události téhož hovoru).
    pub fn with_id(surl: &str, id: &str) -> Self {
        Self {
            base: format!("{surl}callAgent/{id}/"),
            id: id.to_owned(),
        }
    }

    /// `<uuid>` z adres: podle něj se pozná, ke kterému hovoru doručená událost patří.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Adresa pro událost `kind/name` (např. `call/acceptance`); každá má vlastní náhodný úsek.
    pub fn link(&self, kind: &str, name: &str) -> String {
        let tag: u32 = rand::rng().random();
        format!("{}{tag:08x}/{kind}/{name}/", self.base)
    }

    fn links(&self, kind: &str, names: &[&str]) -> Value {
        Value::Object(
            names
                .iter()
                .map(|n| ((*n).to_owned(), json!(self.link(kind, n))))
                .collect(),
        )
    }
}

/// Událost hovoru podle cesty doručení `callAgent/<uuid>/<tag>/<kind>/<name>/`.
pub fn event_name(path: &str) -> Option<String> {
    let mut parts = path.trim_end_matches('/').rsplit('/');
    let name = parts.next()?;
    let kind = parts.next()?;
    path.starts_with("callAgent/")
        .then(|| format!("{kind}/{name}"))
}

/// `<uuid>` z cesty doručení `callAgent/<uuid>/…` (viz [`Callbacks::id`]).
pub fn agent_id(path: &str) -> Option<&str> {
    path.strip_prefix("callAgent/")?.split('/').next()
}

/// Oznámení o příchozím hovoru.
#[derive(Debug, Clone)]
pub struct Invite {
    /// Id hovoru (`debugContent.callId`), zároveň `x-microsoft-skype-chain-id` všech požadavků.
    pub call_id: String,
    pub from: UserId,
    pub from_name: Option<String>,
    /// Volaný účastník (my) tak, jak ho uvádí server: `participantId` použijeme v odpovědích.
    pub participant_id: String,
    pub group: bool,
    pub attach: Url,
    /// Nabídka SDP volajícího (dialekt Microsoftu) a její `mediaLegId`.
    pub offer: Option<(String, String)>,
    /// Odmítnutí před `attach` (po něm platí `callInvitation.links.callLeg`).
    pub reject: Url,
}

/// Rozebere doručení na kořen adresy spojení pro hovory: `{"evt": 107, "gp": base64(JSON)}`.
/// Jiné události (a nečitelná těla) vrací `None`.
pub fn parse_invite(body: &str) -> Option<Invite> {
    use base64::Engine;
    let v: Value = serde_json::from_str(body).ok()?;
    if v.get("evt")?.as_i64()? != 107 {
        return None;
    }
    let raw = v.get("gp")?.as_str()?.trim_end_matches('=');
    let bytes = base64::engine::general_purpose::STANDARD_NO_PAD
        .decode(raw)
        .or_else(|_| base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(raw))
        .ok()?;
    let gp: Value = serde_json::from_slice(&bytes).ok()?;
    let n = gp.get("callNotification")?;
    let from = n.pointer("/from/id")?.as_str()?;
    Some(Invite {
        call_id: gp
            .pointer("/debugContent/callId")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
        from: UserId(from.to_owned()),
        from_name: n
            .pointer("/from/displayName")
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .map(str::to_owned),
        participant_id: n
            .pointer("/to/participantId")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
        group: gp
            .pointer("/conversationInvitation/isMultiParty")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        attach: link(n, "/links/attach").ok()?,
        offer: offer_of(n, "/mediaContent"),
        reject: link(n, "/links/reject").ok()?,
    })
}

/// Tělo `POST <links.attach>`: volaný se ohlásí jako koncový bod, který může hovor přijmout.
/// Bez médií (`requireMediaContent: false`); `end` = kam server pošle konec hovoru.
pub fn attach_body(cb: &Callbacks) -> Value {
    json!({
        "attach": {
            "requireMediaContent": false,
            "links": { "end": cb.link("call", "end") },
            "locationContent": null,
            "networkContent": null,
            "areaContent": null,
        },
        "capabilities": null,
        "endpointCapabilities": 73463,
    })
}

/// Tělo `POST <callInvitation.links.progress>`: u nás zvoní (volající slyší vyzvánění).
pub fn progress_body(me: &Caller) -> Value {
    json!({ "callProgress": { "sender": me.json(), "status": "ringing", "phrase": "ringing" } })
}

/// Tělo `DELETE <callLeg / reject>`: odmítnutí příchozího hovoru (kód 603 jako web klient).
pub fn reject_body() -> Value {
    json!({ "callEnd": {
        "code": 603, "subCode": 0, "phrase": "LocalUserInitiated", "resultCategories": ["Success"],
    } })
}

/// Konec hovoru z události `call/end`: `true` = přijato na jiném zařízení uživatele.
pub fn ended_elsewhere(body: &str) -> bool {
    let v: Value = serde_json::from_str(body).unwrap_or_default();
    let end = v.get("callEnd").unwrap_or(&Value::Null);
    end.get("acceptedElsewhereBy").is_some_and(|b| !b.is_null())
        || end.get("code").and_then(Value::as_i64) == Some(450)
}

fn random_hex(bytes: usize) -> String {
    let mut rng = rand::rng();
    (0..bytes)
        .map(|_| format!("{:02X}", rng.random::<u8>()))
        .collect()
}

/// Syntetická nabídka SDP jen se zvukem, ve tvaru, který posílá web klient
/// (`application/sdp-ngc-1.0`). ICE údaje i otisk certifikátu jsou náhodné: slouží jen
/// k ověření signalizace, žádné spojení se podle nich nenaváže.
pub fn probe_offer() -> String {
    let mut rng = rand::rng();
    let ssrc: u32 = rng.random_range(1_000_000..u32::MAX);
    // Port 9 a adresa 0.0.0.0 znamenají „média vypnutá“ a server hovor odmítne
    // (`DisabledMediaNotSupported`), proto port i adresa vypadají jako skutečné;
    // 192.0.2.0/24 je rozsah vyhrazený pro dokumentaci, nikam nevede.
    let port: u16 = rng.random_range(40_000..60_000);
    let addr = "192.0.2.10";
    let fingerprint = (0..32)
        .map(|_| format!("{:02X}", rng.random::<u8>()))
        .collect::<Vec<_>>()
        .join(":");
    let lines = [
        "v=0".to_owned(),
        format!("o=- {} 2 IN IP4 127.0.0.1", rng.random::<u32>()),
        "s=-".into(),
        "b=CT:4000".into(),
        "t=0 0".into(),
        "a=group:BUNDLE 0".into(),
        format!("m=audio {port} RTP/SAVP 111 0 8 13 126"),
        format!("c=IN IP4 {addr}"),
        "a=x-signaling-fb:* x-message app recv:dsh".into(),
        format!("a=x-ssrc-range:{ssrc}-{ssrc}"),
        "a=rtpmap:111 opus/48000/2".into(),
        "a=rtpmap:0 PCMU/8000".into(),
        "a=rtpmap:8 PCMA/8000".into(),
        "a=rtpmap:13 CN/8000".into(),
        "a=rtpmap:126 telephone-event/8000".into(),
        "a=fmtp:111 minptime=10;useinbandfec=1".into(),
        format!("a=rtcp:{port}"),
        "a=setup:actpass".into(),
        "a=mid:0".into(),
        "a=sendrecv".into(),
        format!("a=ice-ufrag:{}", random_hex(2)),
        format!("a=ice-pwd:{}", random_hex(12)),
        format!("a=fingerprint:sha-256 {fingerprint}"),
        format!(
            "a=candidate:{} 1 udp 2122260223 {addr} {port} typ host",
            rng.random::<u32>()
        ),
        "a=ice-options:trickle".into(),
        format!("a=ssrc:{ssrc} cname:{}", random_hex(8)),
        "a=rtcp-mux".into(),
        "a=label:main-audio".into(),
    ];
    lines.join("\r\n") + "\r\n"
}

/// Tělo `POST …/epconv`: založí konverzaci hovoru a rovnou nabídne média.
pub fn conversation_body(
    caller: &Caller,
    cb: &Callbacks,
    sdp: &str,
    client: &str,
    scenario: Option<&str>,
    video: bool,
) -> Value {
    json!({
        "conversationRequest": {
            "conversationType": null,
            "subject": null,
            "suppressDialout": true,
            "roster": { "type": "Delta", "rosterUpdate": cb.link("conversation", "rosterUpdate") },
            "properties": {
                "allowConversationWithoutHost": true,
                "enableGroupCallEventMessages": true,
                "enableGroupCallUpgradeMessage": false,
                "enableGroupCallMeetupGeneration": false,
            },
            "links": cb.links("conversation", &[
                "conversationEnd", "conversationUpdate", "localParticipantUpdate",
                "addParticipantSuccess", "addParticipantFailure", "addModalitySuccess",
                "addModalityFailure", "confirmUnmute", "receiveMessage",
            ]),
            "scenario": scenario,
        },
        "participants": { "from": caller.json(), "to": [] },
        "endpointMetadata": {},
        "callInvitation": {
            "callModalities": modalities(video),
            "links": cb.links("call", &["progress", "mediaAnswer", "acceptance", "redirection", "end"]),
            "clientContentForMediaController": {
                "controlVideoStreaming": cb.link("call", "controlVideoStreaming"),
                "csrcInfo": cb.link("call", "csrcInfo"),
            },
            "pstnContent": { "emergencyCallCountry": "", "platformName": client, "publicApiCall": false },
            "mediaContent": {
                "blob": sdp,
                "contentType": "application/sdp-ngc-1.0",
                "mediaLegId": random_hex(16),
            },
        },
    })
}

/// Druhy médií hovoru, jak je uvádí signalizace.
fn modalities(video: bool) -> Value {
    if video {
        json!(["Audio", "Video"])
    } else {
        json!(["Audio"])
    }
}

fn media_content(sdp: &str, media_leg_id: &str) -> Value {
    json!({
        "blob": sdp,
        "contentType": "application/sdp-ngc-1.0",
        "mediaLegId": media_leg_id,
    })
}

/// Tělo `POST <callInvitation.links.acceptance>`: přijetí příchozího hovoru s odpovědí SDP.
pub fn acceptance_body(
    me: &Caller,
    cb: &Callbacks,
    sdp: &str,
    media_leg_id: &str,
    video: bool,
) -> Value {
    json!({ "callAcceptance": {
        "acceptedBy": me.json(),
        "acceptedCallModalities": modalities(video),
        "capabilities": null,
        "endpointCapabilities": 73463,
        "links": cb.links("call", &[
            "mediaRenegotiation", "transfer", "replacement", "balanceUpdate",
            "retargetCompletion", "controlVideoStreaming", "updateMediaDescriptions",
        ]),
        "clientContentForMediaController": {
            "controlVideoStreaming": cb.link("call", "controlVideoStreaming"),
            "csrcInfo": cb.link("call", "csrcInfo"),
        },
        "mediaContent": media_content(sdp, media_leg_id),
    } })
}

/// Tělo `POST <mediaNegotiation.links.mediaAnswer>`: odpověď na přeVyjednání médií.
pub fn media_answer_body(
    me: &Caller,
    cb: &Callbacks,
    sdp: &str,
    media_leg_id: &str,
    video: bool,
) -> Value {
    json!({ "mediaAnswer": {
        "callModalities": modalities(video),
        "sender": me.json(),
        "links": { "mediaAcknowledgement": cb.link("call", "mediaAcknowledgement") },
        "clientContentForMediaController": {
            "controlVideoStreaming": cb.link("call", "controlVideoStreaming"),
            "csrcInfo": cb.link("call", "csrcInfo"),
        },
        "mediaContent": media_content(sdp, media_leg_id),
    } })
}

/// Tělo `DELETE <callLeg>`: zavěšení přijatého hovoru.
pub fn hangup_body() -> Value {
    json!({ "callEnd": {
        "code": 0, "subCode": 0, "phrase": "LocalUserInitiated", "resultCategories": ["Success"],
    } })
}

/// SDP nabídka a `mediaLegId` z `…/mediaContent`.
pub fn offer_of(v: &Value, pointer: &str) -> Option<(String, String)> {
    let mc = v.pointer(pointer)?;
    Some((
        mc.get("blob")?.as_str()?.to_owned(),
        mc.get("mediaLegId")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| random_hex(16)),
    ))
}

/// Proč hovor skončil (pro uživatele), z `call/end` nebo `conversation/conversationEnd`.
pub fn end_reason(body: &str) -> String {
    let v: Value = serde_json::from_str(body).unwrap_or_default();
    let phrase = v
        .pointer("/callEnd/phrase")
        .or_else(|| v.get("phrase"))
        .and_then(Value::as_str)
        .unwrap_or("");
    match phrase {
        "" | "LocalUserInitiated" | "CallEndReasonLocalUserInitiated" => String::new(),
        p if p.starts_with("This conversation has ended") => String::new(),
        p => p.to_owned(),
    }
}

/// Proč se protistranu nepodařilo přizvat (`conversation/addParticipantFailure`), česky.
/// Tvar těla není z odchytu známý, proto se hledá kód SIP na několika místech.
pub fn add_failure_reason(v: &Value) -> String {
    let code = ["/addParticipantFailure/code", "/code", "/callEnd/code"]
        .iter()
        .find_map(|p| v.pointer(p).and_then(Value::as_u64));
    match code {
        Some(486) => "protistrana má obsazeno".into(),
        Some(603) => "protistrana hovor odmítla".into(),
        Some(480 | 408) => "protistrana nezvedá".into(),
        Some(404) => "protistrana není dostupná".into(),
        Some(c) => format!("protistranu se nepodařilo zavolat (kód {c})"),
        None => "protistranu se nepodařilo zavolat".into(),
    }
}

/// Tělo `POST <links.addParticipant>`: pozve protistranu do hovoru v 1:1 chatu `thread`.
/// Bez známého chatu (`thread = None`) se `groupChat` neposílá.
pub fn add_body(caller: &Caller, cb: &Callbacks, to: &str, thread: Option<&str>) -> Value {
    json!({
        "disableUnmute": false,
        "participants": {
            "from": caller.json(),
            "to": [{ "id": to, "participantId": uuid::Uuid::new_v4().to_string() }],
        },
        "participantInvitationData": {},
        "replacementDetails": null,
        "groupContext": null,
        "groupChat": thread.map(|t| json!({ "threadId": t, "messageId": null })),
        "links": cb.links("conversation", &["addParticipantSuccess", "addParticipantFailure"]),
    })
}

/// Tělo `POST <links.leave>`: zavěšení volajícím.
pub fn leave_body(caller: &Caller) -> Value {
    json!({
        "participants": { "from": caller.json() },
        "conversationTransactionEnd": { "reason": "noError", "code": 0, "phrase": "ConversationEndNoError" },
        "callTransactionEnd": {
            "code": 0, "subCode": 0, "phrase": "CallEndReasonLocalUserInitiated",
            "resultCategories": ["Success"],
        },
    })
}

/// Odpověď klienta na `call/acceptance`: adresy pro další události přijatého hovoru.
pub fn acceptance_ack(cb: &Callbacks) -> Value {
    json!({ "callAcceptanceAcknowledgement": { "links": cb.links("call", &[
        "mediaRenegotiation", "transfer", "replacement", "balanceUpdate",
        "retargetCompletion", "controlVideoStreaming", "updateMediaDescriptions",
    ]) } })
}

/// Odkaz z odpovědi serveru; tokeny se posílají jen na `https` domény Microsoftu.
pub fn link(v: &Value, pointer: &str) -> Result<Url> {
    let raw = v
        .pointer(pointer)
        .and_then(Value::as_str)
        .ok_or_else(|| Error::Protocol(format!("odpověď hovoru bez {pointer}")))?;
    Url::parse(raw)
        .ok()
        .filter(trusted)
        .ok_or_else(|| Error::Protocol(format!("nedůvěryhodný odkaz {pointer}")))
}

/// Stručný popis SDP (bez adres a klíčů) pro výpis zkoušky.
pub fn describe_sdp(sdp: &str) -> String {
    let lines: Vec<&str> = sdp.lines().collect();
    let media: Vec<&str> = lines
        .iter()
        .filter_map(|l| l.strip_prefix("m=")?.split(' ').next())
        .collect();
    let candidates = lines
        .iter()
        .filter(|l| l.starts_with("a=candidate"))
        .count();
    let setup = lines
        .iter()
        .find_map(|l| l.strip_prefix("a=setup:"))
        .unwrap_or("?");
    format!(
        "{} řádků, média [{}], {candidates} ICE kandidátů, setup:{setup}",
        lines.len(),
        media.join(", ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caller() -> Caller {
        Caller {
            mri: UserId("8:orgid:aaaa".into()),
            display_name: "Jan Novák".into(),
            endpoint_id: "ep-1".into(),
            participant_id: "p-1".into(),
        }
    }

    #[test]
    fn callbacks_point_under_trouter_address() {
        let cb = Callbacks::new("https://pub.example:3443/v4/f/abc/");
        let a = cb.link("call", "acceptance");
        let b = cb.link("call", "end");
        assert!(a.starts_with("https://pub.example:3443/v4/f/abc/callAgent/"));
        assert!(a.ends_with("/call/acceptance/"));
        assert!(b.ends_with("/call/end/"));
        // cestu za surl pozná `event_name`
        let path = a
            .strip_prefix("https://pub.example:3443/v4/f/abc/")
            .unwrap();
        assert_eq!(event_name(path).as_deref(), Some("call/acceptance"));
        assert_eq!(event_name("messaging"), None);
    }

    #[test]
    fn builds_conversation_request() {
        let cb = Callbacks::new("https://pub.example/v4/f/abc/");
        let sdp = probe_offer();
        let b = conversation_body(
            &caller(),
            &cb,
            &sdp,
            "klient",
            Some("UserInitiatedTestCall"),
            false,
        );
        assert_eq!(
            b["conversationRequest"]["scenario"],
            "UserInitiatedTestCall"
        );
        assert_eq!(b["callInvitation"]["callModalities"], json!(["Audio"]));
        assert_eq!(b["participants"]["from"]["id"], "8:orgid:aaaa");
        assert_eq!(b["participants"]["from"]["endpointId"], "ep-1");
        assert_eq!(
            b["callInvitation"]["mediaContent"]["contentType"],
            "application/sdp-ngc-1.0"
        );
        assert_eq!(b["callInvitation"]["mediaContent"]["blob"], sdp);
        for name in [
            "progress",
            "mediaAnswer",
            "acceptance",
            "redirection",
            "end",
        ] {
            let l = b["callInvitation"]["links"][name].as_str().unwrap();
            assert!(l.ends_with(&format!("/call/{name}/")), "{l}");
        }
        assert!(
            b["conversationRequest"]["roster"]["rosterUpdate"]
                .as_str()
                .unwrap()
                .ends_with("/conversation/rosterUpdate/")
        );
    }

    #[test]
    fn offer_is_audio_only_and_random() {
        let a = probe_offer();
        assert!(a.contains(" RTP/SAVP 111 0 8 13 126"));
        assert!(!a.contains("m=audio 9 "), "port 9 = vypnutá média");
        assert!(a.contains("typ host"));
        assert!(a.contains("a=setup:actpass"));
        assert!(a.contains("a=label:main-audio"));
        assert!(a.ends_with("\r\n"));
        assert_ne!(a, probe_offer(), "ICE údaje se nesmí opakovat");
        assert_eq!(
            describe_sdp(&a).split(',').nth(1).unwrap().trim(),
            "média [audio]"
        );
    }

    fn invite_body(gp: &Value) -> String {
        use base64::Engine;
        let gp = base64::engine::general_purpose::STANDARD.encode(gp.to_string());
        json!({ "evt": 107, "gp": gp }).to_string()
    }

    #[test]
    fn parses_incoming_call_notification() {
        let fp = "https://api.flightproxy.teams.microsoft.com/api/v2/ep/cc.example/cc/v1/forked/c1";
        let gp = json!({
            "callNotification": {
                "from": { "id": "8:orgid:bbbb", "displayName": "Eva Svobodová", "participantId": "p-from" },
                "to": { "id": "8:orgid:aaaa", "participantId": "p-to" },
                "links": { "attach": format!("{fp}/attach"), "progress": format!("{fp}/progress"),
                           "reject": format!("{fp}/reject"), "mediaAnswer": "cc://ma" },
                "mediaContent": { "blob": "v=0", "mediaLegId": "LEG1" },
            },
            "conversationInvitation": { "isMultiParty": false },
            "debugContent": { "callId": "call-1" },
        });
        let inv = parse_invite(&invite_body(&gp)).unwrap();
        assert_eq!(inv.call_id, "call-1");
        assert_eq!(inv.from.as_str(), "8:orgid:bbbb");
        assert_eq!(inv.from_name.as_deref(), Some("Eva Svobodová"));
        assert_eq!(inv.participant_id, "p-to");
        assert!(!inv.group);
        assert!(inv.attach.as_str().ends_with("/attach"));
        assert!(inv.reject.as_str().ends_with("/reject"));
        assert_eq!(inv.offer, Some(("v=0".into(), "LEG1".into())));
        // jiná událost, cizí odkaz nebo nečitelné tělo = žádný hovor
        assert!(parse_invite(&json!({"evt": 108, "gp": ""}).to_string()).is_none());
        assert!(parse_invite("<xml/>").is_none());
        let mut evil = gp.clone();
        evil["callNotification"]["links"]["attach"] = json!("https://evil.example/attach");
        assert!(parse_invite(&invite_body(&evil)).is_none());
    }

    #[test]
    fn incoming_call_bodies() {
        let cb = Callbacks::new("https://pub.example/v4/f/abc/");
        let attach = attach_body(&cb);
        let end = attach["attach"]["links"]["end"].as_str().unwrap();
        assert!(end.ends_with("/call/end/"));
        let path = end.strip_prefix("https://pub.example/v4/f/abc/").unwrap();
        assert_eq!(agent_id(path), Some(cb.id()));
        assert_eq!(agent_id("messaging"), None);
        let p = progress_body(&caller());
        assert_eq!(p["callProgress"]["status"], "ringing");
        assert_eq!(p["callProgress"]["sender"]["id"], "8:orgid:aaaa");
        assert_eq!(reject_body()["callEnd"]["code"], 603);
        assert_eq!(hangup_body()["callEnd"]["code"], 0);
        let acc = acceptance_body(&caller(), &cb, "v=0", "LEG", true);
        assert_eq!(
            acc["callAcceptance"]["acceptedCallModalities"],
            json!(["Audio", "Video"])
        );
        assert_eq!(acc["callAcceptance"]["mediaContent"]["mediaLegId"], "LEG");
        assert!(acc["callAcceptance"]["links"]["mediaRenegotiation"].is_string());
        let ans = media_answer_body(&caller(), &cb, "v=0", "LEG", false);
        assert_eq!(ans["mediaAnswer"]["mediaContent"]["blob"], "v=0");
        assert_eq!(
            end_reason(r#"{"callEnd":{"phrase":"LocalUserInitiated"}}"#),
            ""
        );
        assert_eq!(
            end_reason(r#"{"callEnd":{"phrase":"CallTimeout"}}"#),
            "CallTimeout"
        );
        let same = Callbacks::with_id("https://pub.example/v4/f/abc/", cb.id());
        assert!(same.link("call", "end").starts_with(&format!(
            "https://pub.example/v4/f/abc/callAgent/{}/",
            cb.id()
        )));
        assert!(ended_elsewhere(
            r#"{"callEnd":{"code":450,"acceptedElsewhereBy":null}}"#
        ));
        assert!(ended_elsewhere(
            r#"{"callEnd":{"code":0,"acceptedElsewhereBy":{"id":"8:orgid:aaaa"}}}"#
        ));
        assert!(!ended_elsewhere(
            r#"{"callEnd":{"code":0,"phrase":"LocalUserInitiated"}}"#
        ));
    }

    #[test]
    fn turn_credentials_from_trap_tokens() {
        let v = json!({"tokens": [{"realm": "\"rtcmedia\"", "username": "u/1", "password": "p+2"}],
                       "expires": 604800});
        let c = parse_trap_tokens(&v).unwrap();
        assert_eq!(format!("{c:?}"), "TurnCredentials(<skryto>)");
        let urls = c.urls();
        assert!(urls[0].starts_with("turn://u%2F1:p%2B2@euaz.relay.teams.microsoft.com:3478"));
        assert!(urls.iter().any(|u| u.ends_with(":443?transport=tcp")));
        assert!(parse_trap_tokens(&json!({"tokens": []})).is_none());
    }

    #[test]
    fn links_must_be_microsoft_https() {
        let v = json!({"links": {
            "leave": "https://api.flightproxy.teams.microsoft.com/api/v2/ep/x/leave",
            "bad": "https://evil.example/leave", "plain": "http://teams.microsoft.com/x"}});
        assert!(link(&v, "/links/leave").is_ok());
        assert!(link(&v, "/links/bad").is_err());
        assert!(link(&v, "/links/plain").is_err());
        assert!(link(&v, "/links/missing").is_err());
    }

    #[test]
    fn parses_call_logs() {
        // smyšlený záznam ve tvaru `48:calllogs`
        let log = json!({
            "startTime": "2026-03-13T08:55:01.1Z", "connectTime": "2026-03-13T08:55:09.5Z",
            "endTime": "2026-03-13T09:40:54.6Z", "callDirection": "incoming",
            "callType": "twoParty", "callState": "accepted",
            "originator": "8:orgid:bbbb", "target": "8:orgid:aaaa",
            "originatorParticipant": {"id": "8:orgid:bbbb", "displayName": "Jana Nováková"},
            "targetParticipant": {"id": "8:orgid:aaaa", "displayName": null},
            "callId": "c-1"
        });
        let missed = json!({
            "startTime": "2026-03-12T10:00:00Z", "connectTime": null, "endTime": "2026-03-12T10:00:30Z",
            "callDirection": "outgoing", "callType": "twoParty", "callState": "missed",
            "originator": "8:orgid:aaaa", "target": "8:orgid:cccc",
            "targetParticipant": {"id": "8:orgid:cccc", "displayName": "Petr"}, "callId": "c-2"
        });
        let v = json!({"messages": [
            {"messagetype": "Text", "properties": {"call-log": log.to_string()}},
            {"messagetype": "Text", "properties": {"call-log": missed.to_string()}},
            {"messagetype": "Text", "properties": {}},
        ]});
        let r = parse_call_logs(&v, "8:orgid:aaaa");
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].peer.as_str(), "8:orgid:bbbb");
        assert_eq!(r[0].peer_name.as_deref(), Some("Jana Nováková"));
        assert!(r[0].incoming && !r[0].missed());
        assert_eq!(r[0].duration_secs(), Some(2745));
        assert_eq!(r[1].peer.as_str(), "8:orgid:cccc");
        assert!(!r[1].incoming && r[1].missed());
        assert_eq!(r[1].duration_secs(), None);
    }

    #[test]
    fn add_and_leave_bodies() {
        let cb = Callbacks::new("https://pub.example/v4/f/abc/");
        let add = add_body(
            &caller(),
            &cb,
            ECHO_BOT,
            Some("19:aaaa_bbbb@unq.gbl.spaces"),
        );
        assert_eq!(add["participants"]["to"][0]["id"], ECHO_BOT);
        assert_eq!(add["groupChat"]["threadId"], "19:aaaa_bbbb@unq.gbl.spaces");
        // přizvání do přijatého hovoru: chat neznáme, `groupChat` je null
        let more = add_body(&caller(), &cb, "8:orgid:cccc", None);
        assert!(more["groupChat"].is_null());
        assert_eq!(more["participants"]["to"][0]["id"], "8:orgid:cccc");
        let leave = leave_body(&caller());
        assert_eq!(
            leave["callTransactionEnd"]["phrase"],
            "CallEndReasonLocalUserInitiated"
        );
        let ack = acceptance_ack(&cb);
        assert!(ack["callAcceptanceAcknowledgement"]["links"]["mediaRenegotiation"].is_string());
    }

    #[test]
    fn counts_participants_from_roster() {
        let v = serde_json::json!({
            "participants": {"8:orgid:aaaa": {"state": "inactive", "endpoints": {}}},
            "type": "Delta",
            "participantCounts": {"totalParticipants": 1, "lobbyParticipants": 0}
        });
        assert_eq!(participant_count(&v), Some(1));
        assert_eq!(
            participant_count(&serde_json::json!({"participants": {}})),
            None
        );
    }

    #[test]
    fn add_failure_reasons() {
        let r = |v| add_failure_reason(&v);
        assert_eq!(
            r(serde_json::json!({"addParticipantFailure": {"code": 603}})),
            "protistrana hovor odmítla"
        );
        assert_eq!(
            r(serde_json::json!({"code": 486})),
            "protistrana má obsazeno"
        );
        assert_eq!(
            r(serde_json::json!({})),
            "protistranu se nepodařilo zavolat"
        );
    }
}
