//! Signalizace hovorů (`docs/protocol/README.md` § 9). Zatím jen zkouška spojení
//! s echo botem Teams („testovací hovor“): založení, přijetí serverem, ukončení.
//! Média (zvuk, obraz) se nepřenáší: nabídka SDP je syntetická a nikam nevede.

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
}

impl Callbacks {
    pub fn new(surl: &str) -> Self {
        Self {
            base: format!("{surl}callAgent/{}/", uuid::Uuid::new_v4()),
        }
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
pub fn conversation_body(caller: &Caller, cb: &Callbacks, sdp: &str, client: &str) -> Value {
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
            "scenario": "UserInitiatedTestCall",
        },
        "participants": { "from": caller.json(), "to": [] },
        "endpointMetadata": {},
        "callInvitation": {
            "callModalities": ["Audio"],
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

/// Tělo `POST <links.addParticipant>`: pozve protistranu do hovoru v 1:1 chatu `thread`.
pub fn add_body(caller: &Caller, cb: &Callbacks, to: &str, thread: &str) -> Value {
    json!({
        "disableUnmute": false,
        "participants": {
            "from": caller.json(),
            "to": [{ "id": to, "participantId": uuid::Uuid::new_v4().to_string() }],
        },
        "participantInvitationData": {},
        "replacementDetails": null,
        "groupContext": null,
        "groupChat": { "threadId": thread, "messageId": null },
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
        let b = conversation_body(&caller(), &cb, &sdp, "klient");
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
    fn add_and_leave_bodies() {
        let cb = Callbacks::new("https://pub.example/v4/f/abc/");
        let add = add_body(&caller(), &cb, ECHO_BOT, "19:aaaa_bbbb@unq.gbl.spaces");
        assert_eq!(add["participants"]["to"][0]["id"], ECHO_BOT);
        assert_eq!(add["groupChat"]["threadId"], "19:aaaa_bbbb@unq.gbl.spaces");
        let leave = leave_body(&caller());
        assert_eq!(
            leave["callTransactionEnd"]["phrase"],
            "CallEndReasonLocalUserInitiated"
        );
        let ack = acceptance_ack(&cb);
        assert!(ack["callAcceptanceAcknowledgement"]["links"]["mediaRenegotiation"].is_string());
    }
}
