//! Překlad SDP mezi dialektem Microsoftu (`application/sdp-ngc-1.0`, server hovorů Teams)
//! a standardním WebRTC (GStreamer `webrtcbin`). Viz `docs/protocol/README.md` § 9.
//!
//! Microsoft: profil `RTP/SAVP` (i s DTLS), kandidáti `TCP-ACT`/`TCP-PASS` s příponou
//! `MTURNID`, vlastní kodeky (SATIN, SILK, X-H264UC…), řádky `a=x-…` a `a=crypto` (SDES).
//! WebRTC: `UDP/TLS/RTP/SAVPF`, jen DTLS, kandidáti malými písmeny.
//!
//! Server odmítne port 9 / adresu `0.0.0.0` jako „vypnutá média“, proto do odchozího SDP
//! doplníme výchozího kandidáta do `m=` a `c=`.

/// Kodeky, které umíme (zbytek z nabídky Microsoftu zahodíme). Názvy bez ohledu na velikost.
const AUDIO_CODECS: &[&str] = &["opus", "g722", "pcmu", "pcma", "telephone-event", "cn"];
const VIDEO_CODECS: &[&str] = &["h264", "vp8", "rtx"];

/// Jedna sekce SDP: řádky před prvním `m=` (relace) nebo od `m=` do dalšího.
struct Section {
    lines: Vec<String>,
}

fn sections(sdp: &str) -> Vec<Section> {
    let mut out = vec![Section { lines: vec![] }];
    for line in sdp.lines().map(str::trim_end).filter(|l| !l.is_empty()) {
        if line.starts_with("m=") {
            out.push(Section { lines: vec![] });
        }
        out.last_mut()
            .expect("aspoň sekce relace")
            .lines
            .push(line.to_owned());
    }
    out
}

fn join(sections: &[Section]) -> String {
    let mut s = String::new();
    for sec in sections {
        for l in &sec.lines {
            s.push_str(l);
            s.push_str("\r\n");
        }
    }
    s
}

/// `m=<media> <port> <proto> <fmt…>` rozložené.
struct MLine<'a> {
    media: &'a str,
    port: &'a str,
    proto: &'a str,
    fmts: Vec<&'a str>,
}

fn parse_m(line: &str) -> Option<MLine<'_>> {
    let mut p = line.strip_prefix("m=")?.split_whitespace();
    Some(MLine {
        media: p.next()?,
        port: p.next()?,
        proto: p.next()?,
        fmts: p.collect(),
    })
}

/// Payload type a název kodeku z `a=rtpmap:<pt> <název>/<takt>…`.
fn rtpmap(line: &str) -> Option<(&str, &str)> {
    let rest = line.strip_prefix("a=rtpmap:")?;
    let (pt, enc) = rest.split_once(' ')?;
    Some((pt, enc.split('/').next()?))
}

/// PT, kterého se řádek týká (`a=rtpmap:`, `a=fmtp:`, `a=rtcp-fb:`).
fn line_pt(line: &str) -> Option<&str> {
    ["a=rtpmap:", "a=fmtp:", "a=rtcp-fb:"]
        .iter()
        .find_map(|p| line.strip_prefix(p))
        .and_then(|r| r.split([' ', ':']).next())
}

/// Kandidát Microsoftu → WebRTC; `None` = zahodit (TCP, nečitelný).
fn candidate_from_ms(line: &str) -> Option<String> {
    let rest = line.strip_prefix("a=candidate:")?;
    let f: Vec<&str> = rest.split_whitespace().collect();
    // foundation component transport priority addr port "typ" type [raddr x rport y] [MTURNID z]
    if f.len() < 8 || f[6] != "typ" {
        return None;
    }
    if !f[2].eq_ignore_ascii_case("udp") {
        return None;
    }
    let mut out = vec![f[0], f[1], "udp", f[3], f[4], f[5], "typ", f[7]];
    let mut i = 8;
    while i + 1 < f.len() {
        if matches!(f[i], "raddr" | "rport") {
            out.push(f[i]);
            out.push(f[i + 1]);
        }
        i += 2;
    }
    Some(format!("a=candidate:{}", out.join(" ")))
}

/// SDP ze serveru Teams → SDP pro `webrtcbin`.
pub fn from_ms(sdp: &str) -> String {
    let mut secs = sections(sdp);
    let is_direction = |l: &str| DIRECTIONS.contains(&l);
    // směr chybějící u média platí ze session, jinak výchozí sendrecv (RFC 4566);
    // `webrtcbin` ho ale vyžaduje u média (jinak hlásí „Cannot intersect dtls setup“)
    let session_direction = secs
        .first()
        .and_then(|s| s.lines.iter().find(|l| is_direction(l)).cloned())
        .unwrap_or_else(|| "a=sendrecv".to_owned());
    // server u odmítnutého média (port 0) vynechá `a=mid`; `webrtcbin` pak odmítne celou
    // odpověď, proto se mid doplní podle pořadí v `a=group:BUNDLE`
    let bundle: Vec<String> = secs
        .first()
        .and_then(|s| {
            s.lines
                .iter()
                .find_map(|l| l.strip_prefix("a=group:BUNDLE "))
        })
        .map(|g| g.split_whitespace().map(str::to_owned).collect())
        .unwrap_or_default();
    // údaje transportu sdílené v BUNDLE (server je u odmítnutého média vynechá, `webrtcbin`
    // je chce u každého)
    const TRANSPORT: &[&str] = &["a=ice-ufrag:", "a=ice-pwd:", "a=fingerprint:", "a=setup:"];
    let transport: Vec<String> = TRANSPORT
        .iter()
        .filter_map(|p| {
            secs.iter()
                .skip(1)
                .find_map(|s| s.lines.iter().find(|l| l.starts_with(p)).cloned())
        })
        .collect();
    for (i, sec) in secs.iter_mut().enumerate() {
        if i == 0 {
            sec.lines.retain(|l| !l.starts_with("a=x-"));
            continue;
        }
        for t in &transport {
            let prefix = &t[..t.find(':').map_or(t.len(), |i| i + 1)];
            if !sec.lines.iter().any(|l| l.starts_with(prefix)) {
                sec.lines.push(t.clone());
            }
        }
        if !sec.lines.iter().any(|l| l.starts_with("a=mid:"))
            && let Some(mid) = bundle.get(i - 1)
        {
            sec.lines.push(format!("a=mid:{mid}"));
        }
        let Some(m) = parse_m(&sec.lines[0]) else {
            continue;
        };
        let allowed: &[&str] = match m.media {
            "audio" => AUDIO_CODECS,
            "video" => VIDEO_CODECS,
            _ => &[],
        };
        // PT, které necháme: známé kodeky; rtx jen k ponechanému kodeku
        let mut keep: Vec<String> = vec![];
        for l in &sec.lines {
            if let Some((pt, enc)) = rtpmap(l)
                && allowed.iter().any(|a| a.eq_ignore_ascii_case(enc))
                && !enc.eq_ignore_ascii_case("rtx")
            {
                keep.push(pt.to_owned());
            }
        }
        for l in &sec.lines {
            if let Some(apt) = l
                .strip_prefix("a=fmtp:")
                .and_then(|r| r.split_once(' '))
                .and_then(|(pt, p)| Some((pt, p.strip_prefix("apt=")?)))
                && keep.iter().any(|k| k == apt.1.trim())
            {
                keep.push(apt.0.to_owned());
            }
        }
        if keep.is_empty() {
            // médium, které neumíme (např. data `x-data`): odmítnout portem 0
            let fmt = m.fmts.first().copied().unwrap_or("0");
            let mut lines = vec![format!("m={} 0 {} {fmt}", m.media, m.proto)];
            lines.extend(
                sec.lines
                    .iter()
                    .filter(|l| {
                        l.starts_with("a=mid:") || TRANSPORT.iter().any(|p| l.starts_with(p))
                    })
                    .cloned(),
            );
            lines.push("a=inactive".to_owned());
            sec.lines = lines;
            continue;
        }
        let fmts: Vec<&str> = m
            .fmts
            .iter()
            .copied()
            .filter(|f| keep.iter().any(|k| k == f))
            .collect();
        let head = format!(
            "m={} {} UDP/TLS/RTP/SAVPF {}",
            m.media,
            m.port,
            fmts.join(" ")
        );
        let mut lines = vec![head];
        for l in sec.lines.iter().skip(1) {
            if l.starts_with("a=x-")
                || l.starts_with("a=crypto:")
                || l.starts_with("a=extmap:")
                || l.starts_with("a=label:")
                || l.starts_with("a=ssrc-group:")
            {
                continue;
            }
            if let Some(pt) = line_pt(l)
                && !keep.iter().any(|k| k == pt)
            {
                continue;
            }
            if l.starts_with("a=candidate:") {
                if let Some(c) = candidate_from_ms(l) {
                    lines.push(c);
                }
                continue;
            }
            lines.push(l.clone());
        }
        if m.port == "0" {
            lines.retain(|l| !is_direction(l));
            lines.push("a=inactive".to_owned());
        } else if !lines.iter().any(|l| is_direction(l)) {
            lines.push(session_direction.clone());
        }
        sec.lines = lines;
    }
    join(&secs)
}

const DIRECTIONS: &[&str] = &["a=sendrecv", "a=sendonly", "a=recvonly", "a=inactive"];

/// Výchozí kandidát sekce (komponenta 1, nejlépe relay, pak srflx, pak host): `(adresa, port)`.
fn default_candidate(lines: &[String]) -> Option<(String, String)> {
    let mut best: Option<(u8, String, String)> = None;
    for l in lines {
        let Some(rest) = l.strip_prefix("a=candidate:") else {
            continue;
        };
        let f: Vec<&str> = rest.split_whitespace().collect();
        if f.len() < 8 || f[1] != "1" || !f[2].eq_ignore_ascii_case("udp") || f[4].contains(':') {
            continue;
        }
        let rank = match f[7] {
            "relay" => 3,
            "srflx" => 2,
            "host" => 1,
            _ => 0,
        };
        if best.as_ref().is_none_or(|b| rank > b.0) {
            best = Some((rank, f[4].to_owned(), f[5].to_owned()));
        }
    }
    best.map(|(_, a, p)| (a, p))
}

/// Kandidát pro server Microsoftu (`a=candidate:…`), nebo `None`, pokud ho nepřijme:
/// jen UDP přes IPv4, protokol malými písmeny.
fn ms_candidate(c: &str) -> Option<String> {
    let f: Vec<&str> = c.split(' ').collect();
    let (proto, addr) = (f.get(2)?, f.get(4)?);
    if !proto.eq_ignore_ascii_case("udp") || addr.parse::<std::net::Ipv4Addr>().is_err() {
        return None;
    }
    let mut f = f.clone();
    f[2] = "udp";
    Some(format!("a=candidate:{}", f.join(" ")))
}

/// SDP z `webrtcbin` (po sběru kandidátů) → SDP pro server Teams.
pub fn to_ms(sdp: &str) -> String {
    let mut secs = sections(sdp);
    // kandidáti mohou být jen v sekci prvního média (BUNDLE): použít je jako výchozí všude
    let shared = secs
        .iter()
        .skip(1)
        .find_map(|s| default_candidate(&s.lines));
    for (i, sec) in secs.iter_mut().enumerate() {
        if i == 0 {
            continue;
        }
        let Some(m) = parse_m(&sec.lines[0]) else {
            continue;
        };
        // `webrtcbin` (max-bundle) dává dalším médiím port 0 s `a=bundle-only`; server by je
        // bral jako odmítnutá, web klient má všude stejný port
        let bundled = sec.lines.iter().any(|l| l == "a=bundle-only");
        let rejected = m.port == "0" && !bundled;
        let (addr, port) = default_candidate(&sec.lines)
            .or_else(|| shared.clone())
            .unwrap_or_else(|| ("0.0.0.0".into(), m.port.to_owned()));
        let port = if rejected { "0".to_owned() } else { port };
        let proto = if m.proto.contains("RTP") {
            "RTP/SAVP"
        } else {
            m.proto
        };
        let label = match m.media {
            "audio" => Some("main-audio"),
            "video" => Some("main-video"),
            _ => None,
        };
        let ssrcs: Vec<u32> = sec
            .lines
            .iter()
            .filter_map(|l| l.strip_prefix("a=ssrc:")?.split(' ').next()?.parse().ok())
            .collect();
        let mut lines = vec![format!("m={} {port} {proto} {}", m.media, m.fmts.join(" "))];
        let mut has_c = false;
        for l in sec.lines.iter().skip(1) {
            if l.starts_with("c=") {
                has_c = true;
                lines.push(format!("c=IN IP4 {addr}"));
                continue;
            }
            // RTCP na stejném portu (rtcp-mux); odlišný port by server mohl brát vážně
            if l.starts_with("a=rtcp:")
                || l == "a=bundle-only"
                || l.starts_with("a=end-of-candidates")
                || l.starts_with("a=rtcp-mux-only")
            {
                continue;
            }
            if let Some(c) = l.strip_prefix("a=candidate:") {
                // server bere jen UDP přes IPv4 (TCP zapisuje po svém, `tcp-act`)
                if let Some(c) = ms_candidate(c) {
                    lines.push(c);
                }
                continue;
            }
            if let Some(map) = l.strip_prefix("a=rtpmap:")
                && let Some((pt, codec)) = map.split_once(' ')
                && codec.to_ascii_lowercase().starts_with("opus/48000")
            {
                // jako web klient: malými písmeny, s počtem kanálů a s FEC
                lines.push(format!("a=rtpmap:{pt} opus/48000/2"));
                let fmtp = format!("a=fmtp:{pt} ");
                if !sec.lines.iter().any(|l| l.starts_with(&fmtp)) {
                    lines.push(format!("{fmtp}minptime=10;useinbandfec=1"));
                }
                continue;
            }
            if let Some(map) = l.strip_prefix("a=rtpmap:")
                && let Some((pt, codec)) = map.split_once(' ')
                && codec.to_ascii_uppercase().starts_with("H264/")
            {
                lines.push(l.clone());
                // `webrtcbin` u H264 `a=fmtp` nepíše; server ho chce (jako web klient:
                // Constrained Baseline 3.1, fragmentace FU-A)
                let fmtp = format!("a=fmtp:{pt} ");
                if !sec.lines.iter().any(|l| l.starts_with(&fmtp)) {
                    lines.push(format!(
                        "{fmtp}packetization-mode=1;profile-level-id=42e01f;level-asymmetry-allowed=1"
                    ));
                }
                continue;
            }
            lines.push(l.clone());
        }
        if !has_c {
            lines.insert(1, format!("c=IN IP4 {addr}"));
        }
        if !rejected {
            if let (Some(lo), Some(hi)) = (ssrcs.iter().min(), ssrcs.iter().max()) {
                lines.push(format!("a=x-ssrc-range:{lo}-{hi}"));
            }
            if let Some(label) = label {
                lines.push(format!("a=label:{label}"));
            }
        }
        sec.lines = lines;
    }
    join(&secs)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Zkrácená nabídka ve stylu serveru Teams (smyšlené adresy z dokumentačních rozsahů).
    const MS_OFFER: &str = "v=0\r\n\
o=- 0 0 IN IP4 198.51.100.7\r\n\
s=session\r\n\
c=IN IP4 198.51.100.7\r\n\
b=CT:4000\r\n\
t=0 0\r\n\
a=x-mediabw:main-video send=12000;recv=12000\r\n\
a=group:BUNDLE audio_0 video_1\r\n\
m=audio 3480 RTP/SAVP 109 104 102 0 8 13 101\r\n\
a=x-ssrc-range:100-100\r\n\
a=ice-ufrag:abcd\r\n\
a=ice-pwd:efghijklmnopqrstuvwxyz12\r\n\
a=fingerprint:sha-256 AA:BB\r\n\
a=setup:actpass\r\n\
a=candidate:10 1 UDP 1258286591 198.51.100.7 3480 typ relay raddr 203.0.113.5 rport 50000 MTURNID 123\r\n\
a=candidate:13 1 TCP-PASS 1248191998 198.51.100.8 54074 typ relay raddr 203.0.113.5 rport 50002\r\n\
a=crypto:2 AES_CM_128_HMAC_SHA1_80 inline:xxxx\r\n\
a=mid:audio_0\r\n\
a=rtpmap:109 SATINFB/48000/2\r\n\
a=fmtp:109 decoderversion=1\r\n\
a=rtpmap:104 SILK/16000\r\n\
a=rtpmap:102 opus/48000/2\r\n\
a=fmtp:102 useinbandfec=1; minptime=10\r\n\
a=rtpmap:0 PCMU/8000\r\n\
a=rtpmap:8 PCMA/8000\r\n\
a=rtpmap:13 CN/8000\r\n\
a=rtpmap:101 telephone-event/8000\r\n\
a=rtcp-mux\r\n\
a=extmap:1 http:\\\\www.webrtc.org\\experiments\\rtp-hdrext\\abs-send-time\r\n\
m=video 3480 RTP/SAVP 119 100 107 99\r\n\
a=mid:video_1\r\n\
a=recvonly\r\n\
a=rtpmap:119 X-MSAV1/90000\r\n\
a=rtpmap:100 rtx/90000\r\n\
a=fmtp:100 apt=119\r\n\
a=rtpmap:107 H264/90000\r\n\
a=fmtp:107 profile-level-id=42C02A;packetization-mode=1\r\n\
a=rtpmap:99 rtx/90000\r\n\
a=fmtp:99 apt=107\r\n\
a=rtcp-mux\r\n\
m=application 0 RTP/SAVP 127\r\n\
a=mid:data_2\r\n\
a=rtpmap:127 x-data/90000\r\n";

    #[test]
    fn ms_offer_becomes_webrtc() {
        let s = from_ms(MS_OFFER);
        assert!(
            s.contains("m=audio 3480 UDP/TLS/RTP/SAVPF 102 0 8 13 101\r\n"),
            "{s}"
        );
        assert!(
            s.contains("m=video 3480 UDP/TLS/RTP/SAVPF 107 99\r\n"),
            "{s}"
        );
        assert!(
            s.contains("m=application 0 RTP/SAVP 127\r\na=mid:data_2\r\n"),
            "{s}"
        );
        assert!(!s.contains("SATIN") && !s.contains("SILK") && !s.contains("X-MSAV1"));
        assert!(!s.contains("a=fmtp:100"), "rtx k zahozenému kodeku");
        assert!(s.contains("a=fmtp:99 apt=107"));
        assert!(!s.contains("a=x-") && !s.contains("a=crypto") && !s.contains("extmap"));
        assert!(s.contains(
            "a=candidate:10 1 udp 1258286591 198.51.100.7 3480 typ relay raddr 203.0.113.5 rport 50000\r\n"
        ));
        assert!(!s.contains("TCP-PASS") && !s.contains("MTURNID"));
        assert!(s.contains("a=fingerprint:sha-256 AA:BB") && s.contains("a=setup:actpass"));
        assert!(s.contains("a=group:BUNDLE audio_0 video_1"));
        // směr: u zvuku chyběl → sendrecv, u videa zůstane jen recvonly
        let audio = s.split("m=video").next().unwrap();
        assert!(audio.contains("a=sendrecv\r\n"), "{audio}");
        assert!(!s.split("m=video").nth(1).unwrap().contains("a=sendrecv"));
    }

    /// Odpověď ve stylu `webrtcbin` po sběru kandidátů.
    const WEBRTC_ANSWER: &str = "v=0\r\n\
o=- 123 0 IN IP4 0.0.0.0\r\n\
s=-\r\n\
t=0 0\r\n\
a=group:BUNDLE audio_0 video_1\r\n\
m=audio 9 UDP/TLS/RTP/SAVPF 102\r\n\
c=IN IP4 0.0.0.0\r\n\
a=ice-ufrag:wxyz\r\n\
a=ice-pwd:0123456789abcdefghijklmn\r\n\
a=fingerprint:sha-256 CC:DD\r\n\
a=setup:active\r\n\
a=mid:audio_0\r\n\
a=sendrecv\r\n\
a=rtcp-mux\r\n\
a=rtpmap:102 OPUS/48000/2\r\n\
a=ssrc:4000000001 cname:x\r\n\
a=candidate:1 1 UDP 2015363327 192.168.1.20 40000 typ host\r\n\
a=candidate:2 1 UDP 1015021823 203.0.113.9 41000 typ srflx raddr 192.168.1.20 rport 40000\r\n\
a=candidate:3 1 TCP 1015021823 192.168.1.20 9 typ host tcptype active\r\n\
a=candidate:4 1 UDP 2015364095 fe80::1 50065 typ host\r\n\
a=rtcp-mux-only\r\n\
a=end-of-candidates\r\n\
m=video 9 UDP/TLS/RTP/SAVPF 107 99\r\n\
c=IN IP4 0.0.0.0\r\n\
a=mid:video_1\r\n\
a=recvonly\r\n\
a=rtcp-mux\r\n\
a=rtpmap:107 H264/90000\r\n\
a=rtpmap:99 rtx/90000\r\n\
a=fmtp:99 apt=107\r\n\
m=application 0 RTP/SAVP 127\r\n\
a=mid:data_2\r\n";

    #[test]
    fn webrtc_answer_becomes_ms() {
        let s = to_ms(WEBRTC_ANSWER);
        // výchozí kandidát = srflx (bez relay), sdílený i pro video v BUNDLE
        assert!(
            s.contains("m=audio 41000 RTP/SAVP 102\r\nc=IN IP4 203.0.113.9\r\n"),
            "{s}"
        );
        assert!(
            s.contains("m=video 41000 RTP/SAVP 107 99\r\nc=IN IP4 203.0.113.9\r\n"),
            "{s}"
        );
        assert!(
            s.contains("m=application 0 RTP/SAVP 127\r\nc=IN IP4"),
            "{s}"
        );
        assert!(s.contains("a=x-ssrc-range:4000000001-4000000001\r\na=label:main-audio\r\n"));
        assert!(s.contains("a=label:main-video"));
        assert!(!s.contains("end-of-candidates"));
        assert!(!s.contains(" 9 UDP/TLS"));
        // server bere jen UDP přes IPv4; Opus zapisuje jako web klient
        assert!(s.contains("a=candidate:1 1 udp 2015363327 192.168.1.20 40000 typ host\r\n"));
        assert!(!s.contains("tcptype") && !s.contains("fe80") && !s.contains("rtcp-mux-only"));
        assert!(
            s.contains("a=rtpmap:102 opus/48000/2\r\na=fmtp:102 minptime=10;useinbandfec=1\r\n")
        );
    }

    /// Videohovor z `webrtcbin` (max-bundle): video má port 0 a `a=bundle-only`.
    #[test]
    fn bundle_only_video_gets_shared_port() {
        let sdp = "v=0\r\no=- 1 0 IN IP4 0.0.0.0\r\ns=-\r\nt=0 0\r\n\
a=group:BUNDLE audio0 video1\r\n\
m=audio 9 UDP/TLS/RTP/SAVPF 111\r\nc=IN IP4 0.0.0.0\r\na=mid:audio0\r\n\
a=candidate:1 1 UDP 100 198.51.100.3 3478 typ relay raddr 203.0.113.9 rport 1\r\n\
a=rtpmap:111 OPUS/48000/2\r\n\
m=video 0 UDP/TLS/RTP/SAVPF 107\r\nc=IN IP4 0.0.0.0\r\na=bundle-only\r\na=mid:video1\r\n\
a=sendrecv\r\na=rtpmap:107 H264/90000\r\na=ssrc:42 cname:x\r\n";
        let s = to_ms(sdp);
        assert!(
            s.contains("m=video 3478 RTP/SAVP 107\r\nc=IN IP4 198.51.100.3\r\n"),
            "{s}"
        );
        assert!(!s.contains("bundle-only"), "{s}");
        assert!(
            s.contains("a=x-ssrc-range:42-42\r\na=label:main-video\r\n"),
            "{s}"
        );
        assert!(
            s.contains("a=rtpmap:107 H264/90000\r\na=fmtp:107 packetization-mode=1;profile-level-id=42e01f;level-asymmetry-allowed=1\r\n"),
            "{s}"
        );
    }

    /// Server video odmítne (echo bot): port 0, `RTP/AVP`, bez `a=mid` a bez směru.
    #[test]
    fn rejected_video_gets_mid_from_bundle() {
        let answer = "v=0\r\no=- 1 0 IN IP4 198.51.100.7\r\ns=session\r\nt=0 0\r\n\
a=group:BUNDLE audio0 video1\r\na=x-plaza-msi-range:1-100 101-200\r\n\
m=audio 3478 RTP/SAVP 111\r\nc=IN IP4 198.51.100.7\r\na=setup:passive\r\na=mid:audio0\r\n\
a=rtpmap:111 OPUS/48000/2\r\n\
m=video 0 RTP/AVP 107\r\na=label:main-video\r\n";
        let s = from_ms(answer);
        let video = s.split("m=video").nth(1).expect("video");
        assert!(video.contains("a=mid:video1\r\n"), "{s}");
        assert!(video.contains("a=inactive\r\n"), "{s}");
        assert!(!video.contains("a=sendrecv"), "{s}");
        let audio = s.split("m=video").next().unwrap();
        assert!(
            audio.contains("a=mid:audio0\r\n") && audio.contains("a=sendrecv"),
            "{s}"
        );
    }

    #[test]
    fn candidates_prefer_relay() {
        let lines: Vec<String> = [
            "a=candidate:1 1 udp 1 192.168.1.20 40000 typ host",
            "a=candidate:2 1 udp 1 198.51.100.3 3478 typ relay raddr 203.0.113.9 rport 1",
            "a=candidate:3 1 udp 1 2001:db8::1 40000 typ host",
            "a=candidate:4 2 udp 1 198.51.100.4 3479 typ relay",
        ]
        .map(String::from)
        .to_vec();
        assert_eq!(
            default_candidate(&lines),
            Some(("198.51.100.3".into(), "3478".into()))
        );
    }
}
