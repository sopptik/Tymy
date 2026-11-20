//! Média hovorů nad GStreamerem: `webrtcbin` (ICE, DTLS-SRTP, relay servery Teams),
//! zvuk Opus z mikrofonu (PipeWire/PulseAudio) do reproduktoru.
//!
//! SDP se vyjednává ve standardním tvaru WebRTC; překlad do dialektu Microsoftu dělá
//! `tymy-api`. Kandidáti ICE se nesbírají průběžně (Teams trickle nepoužívá): nabídka
//! i odpověď se vrací až po dokončeném sběru.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use gst::prelude::*;
use tymy_core::{Error, Result};

/// Nejdéle, kolik se čeká na sběr kandidátů ICE (relay servery bývají do 1–2 s).
const GATHER_TIMEOUT: Duration = Duration::from_secs(10);
/// Typ obsahu Opus, když ho určujeme my (nabídka).
const OPUS_PT: u32 = 111;

/// Odkud bere hovor zvuk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Microphone,
    /// Testovací tón 440 Hz (zkouška bez mikrofonu).
    TestTone,
}

/// Kam jde zvuk protistrany.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sink {
    Speaker,
    /// Nikam, jen se měří hlasitost (zkouška bez reproduktoru).
    Meter,
}

#[derive(Debug, Clone)]
pub struct Config {
    /// Adresy `turn://jméno:heslo@host:port?transport=…`.
    pub turn: Vec<String>,
    pub source: Source,
    pub sink: Sink,
}

/// Co se za hovor přeneslo.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Stats {
    pub packets_sent: u64,
    pub packets_received: u64,
    /// Nejvyšší hlasitost přijatého zvuku (dBFS; `None` = nic nepřišlo).
    pub peak_db: Option<f64>,
    /// Kolik měření hlasitosti bylo nad prahem ticha.
    pub loud_samples: u64,
    /// Nejvyšší hlasitost od minulého volání `stats` (dBFS).
    pub recent_db: Option<f64>,
}

/// Práh, nad kterým přijatý zvuk nepovažujeme za ticho (dBFS).
const SILENCE_DB: f64 = -50.0;

/// Odpověď na akční signál `webrtcbin`.
struct Reply(tokio::sync::oneshot::Receiver<Option<gst::Structure>>);

impl Reply {
    async fn get(self) -> Option<gst::Structure> {
        self.0.await.ok().flatten()
    }
}

#[derive(Default)]
struct Level {
    peak: Option<f64>,
    loud: u64,
    recent: Option<f64>,
}

struct Shared {
    /// (index m-řádku, řádek kandidáta bez `a=`)
    candidates: Mutex<Vec<(u32, String)>>,
    gathered: tokio::sync::watch::Sender<bool>,
    level: Mutex<Level>,
    /// Nejvyšší hlášené počty paketů (statistika proudu po čase zmizí).
    packets: Mutex<(u64, u64)>,
    stop: AtomicBool,
}

pub struct GstMedia {
    pipeline: gst::Pipeline,
    webrtc: gst::Element,
    config: Config,
    shared: Arc<Shared>,
    /// Vysílací větev zvuku už je napojená (přeVyjednání ji nezakládá znovu).
    sending: Mutex<bool>,
}

fn err(what: &str, e: impl std::fmt::Display) -> Error {
    Error::Other(format!("{what}: {e}"))
}

impl GstMedia {
    pub fn new(config: Config) -> Result<Self> {
        gst::init().map_err(|e| err("GStreamer", e))?;
        let pipeline = gst::Pipeline::with_name("hovor");
        let webrtc = gst::ElementFactory::make("webrtcbin")
            .name("webrtc")
            .property_from_str("bundle-policy", "max-bundle")
            .build()
            .map_err(|e| err("webrtcbin (chybí gstreamer1.0-plugins-bad?)", e))?;
        for url in &config.turn {
            if !webrtc.emit_by_name::<bool>("add-turn-server", &[url]) {
                tracing::warn!("relay server odmítnut");
            }
        }
        pipeline
            .add(&webrtc)
            .map_err(|e| err("sestavení pipeline", e))?;
        let (gathered, _) = tokio::sync::watch::channel(false);
        let shared = Arc::new(Shared {
            candidates: Mutex::new(vec![]),
            gathered,
            level: Mutex::new(Level::default()),
            packets: Mutex::new((0, 0)),
            stop: AtomicBool::new(false),
        });
        {
            let shared = shared.clone();
            webrtc.connect("on-ice-candidate", false, move |vals| {
                let mline = vals[1].get::<u32>().unwrap_or(0);
                if let Ok(c) = vals[2].get::<String>() {
                    let c = c.strip_prefix("a=").unwrap_or(&c).to_owned();
                    shared.candidates.lock().expect("zámek").push((mline, c));
                }
                None
            });
        }
        {
            let shared = shared.clone();
            webrtc.connect_notify(Some("ice-gathering-state"), move |w, _| {
                let s = w.property::<gst_webrtc::WebRTCICEGatheringState>("ice-gathering-state");
                tracing::debug!(?s, "sběr kandidátů ICE");
                if s == gst_webrtc::WebRTCICEGatheringState::Complete {
                    shared.gathered.send_replace(true);
                }
            });
        }
        webrtc.connect_notify(Some("ice-connection-state"), |w, _| {
            let s = w.property::<gst_webrtc::WebRTCICEConnectionState>("ice-connection-state");
            tracing::info!(?s, "spojení ICE");
        });
        webrtc.connect_notify(Some("connection-state"), |w, _| {
            let s = w.property::<gst_webrtc::WebRTCPeerConnectionState>("connection-state");
            tracing::info!(?s, "spojení médií");
        });
        {
            let pipeline = pipeline.downgrade();
            let sink = config.sink;
            webrtc.connect_pad_added(move |_, pad| {
                if pad.direction() != gst::PadDirection::Src {
                    return;
                }
                if let Some(p) = pipeline.upgrade()
                    && let Err(e) = on_remote_pad(&p, pad, sink)
                {
                    tracing::warn!(error = %e, "příchozí médium");
                }
            });
        }
        spawn_bus_watch(&pipeline, shared.clone())?;
        Ok(Self {
            pipeline,
            webrtc,
            config,
            shared,
            sending: Mutex::new(false),
        })
    }

    /// Ztlumí nebo zapne mikrofon.
    pub fn set_muted(&self, muted: bool) {
        if let Some(v) = self.pipeline.by_name("mic") {
            v.set_property("mute", muted);
        }
    }

    pub fn is_muted(&self) -> bool {
        self.pipeline
            .by_name("mic")
            .is_some_and(|v| v.property::<bool>("mute"))
    }

    /// Přenesené pakety a hlasitost přijatého zvuku.
    pub async fn stats(&self) -> Stats {
        let mut s = {
            let mut l = self.shared.level.lock().expect("zámek");
            Stats {
                peak_db: l.peak,
                loud_samples: l.loud,
                recent_db: l.recent.take(),
                ..Default::default()
            }
        };
        if let Some(reply) = self.call("get-stats", &[&None::<gst::Pad>]).get().await {
            for (_, v) in reply.iter() {
                let Ok(st) = v.get::<gst::Structure>() else {
                    continue;
                };
                let field = |n: &str| {
                    st.get::<u64>(n)
                        .ok()
                        .or_else(|| st.get::<u32>(n).ok().map(u64::from))
                };
                match st.name().as_str() {
                    // jeden proud na směr; víc záznamů téhož proudu se nesčítá
                    "outbound-rtp" => {
                        s.packets_sent = s.packets_sent.max(field("packets-sent").unwrap_or(0))
                    }
                    "inbound-rtp" => {
                        s.packets_received = s
                            .packets_received
                            .max(field("packets-received").unwrap_or(0))
                    }
                    _ => {}
                }
            }
        }
        let mut max = self.shared.packets.lock().expect("zámek");
        max.0 = max.0.max(s.packets_sent);
        max.1 = max.1.max(s.packets_received);
        (s.packets_sent, s.packets_received) = *max;
        s
    }

    /// Zastaví zvuk i spojení.
    pub fn stop(&self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        let _ = self.pipeline.set_state(gst::State::Null);
    }

    /// Zavolá akční signál `webrtcbin` s `GstPromise` na konci; odpověď přijde kanálem
    /// (signál se volá synchronně, aby hodnoty GLib nepřežily `await`).
    fn call(&self, signal: &str, args: &[&dyn ToValue]) -> Reply {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let promise = gst::Promise::with_change_func(move |reply| {
            let _ = tx.send(reply.ok().flatten().map(|s| s.to_owned()));
        });
        let mut values: Vec<gst::glib::Value> = args.iter().map(|a| a.to_value()).collect();
        values.push(promise.to_value());
        let refs: Vec<&dyn ToValue> = values.iter().map(|v| v as &dyn ToValue).collect();
        self.webrtc.emit_by_name::<()>(signal, &refs);
        Reply(rx)
    }

    /// Napojí vysílání zvuku (zdroj → Opus → RTP) na `webrtcbin`.
    fn add_audio_sender(&self, pt: u32) -> Result<()> {
        let mut sending = self.sending.lock().expect("zámek");
        if *sending {
            return Ok(());
        }
        let src = match self.config.source {
            Source::TestTone => gst::ElementFactory::make("audiotestsrc")
                .property("is-live", true)
                .property("volume", 0.3f64)
                .build(),
            Source::Microphone => gst::ElementFactory::make("pulsesrc")
                .build()
                .or_else(|_| gst::ElementFactory::make("autoaudiosrc").build()),
        }
        .map_err(|e| err("zdroj zvuku", e))?;
        let make = |f: &str| gst::ElementFactory::make(f).build().map_err(|e| err(f, e));
        let convert = make("audioconvert")?;
        let resample = make("audioresample")?;
        let volume = gst::ElementFactory::make("volume")
            .name("mic")
            .build()
            .map_err(|e| err("volume", e))?;
        let queue = make("queue")?;
        let enc = gst::ElementFactory::make("opusenc")
            .property("bitrate", 32000i32)
            .property("inband-fec", true)
            .build()
            .map_err(|e| err("opusenc", e))?;
        // pevné SSRC: server ho chce znát předem (`a=ssrc`, `a=x-ssrc-range`)
        let ssrc = random_ssrc();
        let pay = gst::ElementFactory::make("rtpopuspay")
            .property("pt", pt)
            .property("ssrc", ssrc)
            .build()
            .map_err(|e| err("rtpopuspay", e))?;
        let caps = gst::Caps::builder("application/x-rtp")
            .field("media", "audio")
            .field("encoding-name", "OPUS")
            .field("clock-rate", 48000i32)
            .field("payload", pt as i32)
            .field("encoding-params", "2")
            .field("ssrc", ssrc)
            .build();
        let filter = gst::ElementFactory::make("capsfilter")
            .property("caps", &caps)
            .build()
            .map_err(|e| err("capsfilter", e))?;
        let chain = [
            &src, &convert, &resample, &volume, &queue, &enc, &pay, &filter,
        ];
        self.pipeline
            .add_many(chain)
            .map_err(|e| err("sestavení pipeline", e))?;
        gst::Element::link_many(chain).map_err(|e| err("propojení zvuku", e))?;
        let sink = self
            .webrtc
            .request_pad_simple("sink_%u")
            .ok_or_else(|| Error::Other("webrtcbin nedal vstup".into()))?;
        filter
            .static_pad("src")
            .expect("capsfilter má výstup")
            .link(&sink)
            .map_err(|e| err("napojení zvuku", e))?;
        if let Some(t) = sink.property::<Option<gst_webrtc::WebRTCRTPTransceiver>>("transceiver") {
            t.set_property(
                "direction",
                gst_webrtc::WebRTCRTPTransceiverDirection::Sendrecv,
            );
        }
        for e in chain {
            e.sync_state_with_parent()
                .map_err(|e| err("spuštění zvuku", e))?;
        }
        *sending = true;
        Ok(())
    }

    fn play(&self) -> Result<()> {
        self.pipeline
            .set_state(gst::State::Playing)
            .map(|_| ())
            .map_err(|e| err("spuštění pipeline", e))
    }

    /// Nastaví vlastní popis a vrátí ho, až budou sebraní kandidáti.
    async fn finish_local(&self, desc: gst_webrtc::WebRTCSessionDescription) -> Result<String> {
        let mut rx = self.shared.gathered.subscribe();
        let done = self.call("set-local-description", &[&desc]);
        done.get().await;
        if tokio::time::timeout(GATHER_TIMEOUT, rx.wait_for(|g| *g))
            .await
            .is_err()
        {
            tracing::warn!("sběr kandidátů ICE nedoběhl včas, posílám, co je");
        }
        let local = self
            .webrtc
            .property::<Option<gst_webrtc::WebRTCSessionDescription>>("local-description")
            .unwrap_or(desc);
        let text = local.sdp().as_text().map_err(|e| err("SDP", e))?;
        let cands = self.shared.candidates.lock().expect("zámek").clone();
        Ok(inject_candidates(&text, &cands))
    }

    async fn set_remote(&self, kind: gst_webrtc::WebRTCSDPType, sdp: &str) -> Result<()> {
        let msg = gst_sdp::SDPMessage::parse_buffer(sdp.as_bytes())
            .map_err(|e| err("SDP protistrany", e))?;
        let desc = gst_webrtc::WebRTCSessionDescription::new(kind, msg);
        let done = self.call("set-remote-description", &[&desc]);
        let reply = done.get().await;
        if let Some(e) = reply
            .as_ref()
            .and_then(|r| r.get::<gst::glib::Error>("error").ok())
        {
            return Err(err("SDP protistrany odmítnuto", e));
        }
        Ok(())
    }
}

#[async_trait]
impl tymy_core::Media for GstMedia {
    async fn offer(&self) -> Result<String> {
        self.add_audio_sender(OPUS_PT)?;
        self.play()?;
        let pending = self.call("create-offer", &[&None::<gst::Structure>]);
        let reply = pending
            .get()
            .await
            .ok_or_else(|| Error::Other("webrtcbin nevytvořil nabídku".into()))?;
        let offer = reply
            .get::<gst_webrtc::WebRTCSessionDescription>("offer")
            .map_err(|e| err("nabídka", e))?;
        self.finish_local(offer).await
    }

    async fn answer(&self, offer: &str) -> Result<String> {
        // vysílat stejným typem obsahu, jaký protistrana nabídla
        let pt = opus_pt(offer).unwrap_or(OPUS_PT);
        self.add_audio_sender(pt)?;
        self.play()?;
        self.set_remote(gst_webrtc::WebRTCSDPType::Offer, offer)
            .await?;
        let pending = self.call("create-answer", &[&None::<gst::Structure>]);
        let reply = pending
            .get()
            .await
            .ok_or_else(|| Error::Other("webrtcbin nevytvořil odpověď".into()))?;
        let answer = reply
            .get::<gst_webrtc::WebRTCSessionDescription>("answer")
            .map_err(|e| err("odpověď", e))?;
        self.finish_local(answer).await
    }

    async fn accept_answer(&self, answer: &str) -> Result<()> {
        self.set_remote(gst_webrtc::WebRTCSDPType::Answer, answer)
            .await
    }

    fn sends_video(&self) -> bool {
        false
    }
}

impl Drop for GstMedia {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Příchozí RTP z `webrtcbin`: dekódovat a zvuk poslat do reproduktoru (nebo jen měřit).
fn on_remote_pad(pipeline: &gst::Pipeline, pad: &gst::Pad, sink: Sink) -> Result<()> {
    let decode = gst::ElementFactory::make("decodebin")
        .build()
        .map_err(|e| err("decodebin", e))?;
    pipeline.add(&decode).map_err(|e| err("pipeline", e))?;
    let weak = pipeline.downgrade();
    decode.connect_pad_added(move |_, pad| {
        let Some(pipeline) = weak.upgrade() else {
            return;
        };
        let kind = pad
            .current_caps()
            .and_then(|c| c.structure(0).map(|s| s.name().to_string()))
            .unwrap_or_default();
        let r = if kind.starts_with("audio/") {
            audio_out(&pipeline, pad, sink)
        } else {
            // obraz zatím nikam (zobrazení přijde s videem)
            discard(&pipeline, pad)
        };
        if let Err(e) = r {
            tracing::warn!(error = %e, kind, "výstup média");
        }
    });
    decode
        .sync_state_with_parent()
        .map_err(|e| err("decodebin", e))?;
    pad.link(&decode.static_pad("sink").expect("decodebin má vstup"))
        .map_err(|e| err("napojení příchozího média", e))?;
    Ok(())
}

fn audio_out(pipeline: &gst::Pipeline, pad: &gst::Pad, sink: Sink) -> Result<()> {
    let make = |f: &str| gst::ElementFactory::make(f).build().map_err(|e| err(f, e));
    let queue = make("queue")?;
    let convert = make("audioconvert")?;
    let resample = make("audioresample")?;
    let level = gst::ElementFactory::make("level")
        .property("interval", 100_000_000u64)
        .property("post-messages", true)
        .build()
        .map_err(|e| err("level", e))?;
    let out = match sink {
        Sink::Speaker => make("autoaudiosink")?,
        Sink::Meter => gst::ElementFactory::make("fakesink")
            .property("sync", false)
            .build()
            .map_err(|e| err("fakesink", e))?,
    };
    let chain = [&queue, &convert, &resample, &level, &out];
    pipeline.add_many(chain).map_err(|e| err("pipeline", e))?;
    gst::Element::link_many(chain).map_err(|e| err("propojení zvuku", e))?;
    for e in chain {
        e.sync_state_with_parent()
            .map_err(|e| err("spuštění zvuku", e))?;
    }
    pad.link(&queue.static_pad("sink").expect("queue má vstup"))
        .map_err(|e| err("napojení zvuku", e))?;
    Ok(())
}

fn discard(pipeline: &gst::Pipeline, pad: &gst::Pad) -> Result<()> {
    let sink = gst::ElementFactory::make("fakesink")
        .property("sync", false)
        .build()
        .map_err(|e| err("fakesink", e))?;
    pipeline.add(&sink).map_err(|e| err("pipeline", e))?;
    sink.sync_state_with_parent()
        .map_err(|e| err("fakesink", e))?;
    pad.link(&sink.static_pad("sink").expect("fakesink má vstup"))
        .map_err(|e| err("napojení", e))?;
    Ok(())
}

/// Zprávy pipeline (chyby, hlasitost přijatého zvuku) ve vlastním vlákně.
fn spawn_bus_watch(pipeline: &gst::Pipeline, shared: Arc<Shared>) -> Result<()> {
    let bus = pipeline
        .bus()
        .ok_or_else(|| Error::Other("pipeline bez sběrnice".into()))?;
    std::thread::Builder::new()
        .name("tymy-media-bus".into())
        .spawn(move || {
            while !shared.stop.load(Ordering::Relaxed) {
                let Some(msg) = bus.timed_pop(gst::ClockTime::from_mseconds(200)) else {
                    continue;
                };
                match msg.view() {
                    gst::MessageView::Error(e) => {
                        tracing::warn!(error = %e.error(), debug = ?e.debug(), "chyba médií");
                    }
                    gst::MessageView::Warning(w) => {
                        tracing::debug!(warning = %w.error(), "varování médií");
                    }
                    gst::MessageView::Element(e) => {
                        if let Some(s) = e.structure().filter(|s| s.name() == "level")
                            && let Some(db) = loudest(s)
                        {
                            let mut l = shared.level.lock().expect("zámek");
                            l.peak = Some(l.peak.map_or(db, |p| p.max(db)));
                            l.recent = Some(l.recent.map_or(db, |p| p.max(db)));
                            if db > SILENCE_DB {
                                l.loud += 1;
                            }
                        }
                    }
                    _ => {}
                }
            }
        })
        .map_err(|e| err("vlákno médií", e))?;
    Ok(())
}

/// Nejhlasitější kanál ze zprávy `level` (pole `rms` v dBFS).
fn loudest(s: &gst::StructureRef) -> Option<f64> {
    let rms = s.get::<gst::glib::ValueArray>("rms").ok()?;
    rms.iter()
        .filter_map(|v| v.get::<f64>().ok())
        .reduce(f64::max)
}

fn random_ssrc() -> u32 {
    use std::hash::{BuildHasher, Hasher};
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u128(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
    );
    (h.finish() as u32).max(1)
}

/// Typ obsahu Opus z nabídky protistrany.
fn opus_pt(sdp: &str) -> Option<u32> {
    sdp.lines().find_map(|l| {
        let rest = l.strip_prefix("a=rtpmap:")?;
        let (pt, codec) = rest.split_once(' ')?;
        codec
            .to_ascii_lowercase()
            .starts_with("opus/")
            .then(|| pt.parse().ok())
            .flatten()
    })
}

/// Doplní kandidáty ICE do SDP, pokud v něm ještě nejsou (podle indexu m-řádku).
fn inject_candidates(sdp: &str, candidates: &[(u32, String)]) -> String {
    let eol = if sdp.contains("\r\n") { "\r\n" } else { "\n" };
    let mut out = String::new();
    let mut mline: Option<u32> = None;
    let present = |m: u32, sdp: &str| {
        sdp.split("m=")
            .nth(m as usize + 1)
            .is_some_and(|s| s.contains("a=candidate:"))
    };
    let flush = |out: &mut String, m: Option<u32>| {
        if let Some(m) = m
            && !present(m, sdp)
        {
            for (_, c) in candidates.iter().filter(|(i, _)| *i == m) {
                out.push_str("a=");
                out.push_str(c);
                out.push_str(eol);
            }
        }
    };
    for line in sdp.split(eol).filter(|l| !l.is_empty()) {
        if line.starts_with("m=") {
            flush(&mut out, mline);
            mline = Some(mline.map_or(0, |m| m + 1));
        }
        out.push_str(line);
        out.push_str(eol);
    }
    flush(&mut out, mline);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SDP: &str = "v=0\r\no=- 1 0 IN IP4 0.0.0.0\r\ns=-\r\nt=0 0\r\n\
m=audio 9 UDP/TLS/RTP/SAVPF 102\r\nc=IN IP4 0.0.0.0\r\na=rtpmap:102 OPUS/48000/2\r\n\
m=video 9 UDP/TLS/RTP/SAVPF 107\r\na=rtpmap:107 H264/90000\r\n";

    #[test]
    fn finds_opus_payload_type() {
        assert_eq!(opus_pt(SDP), Some(102));
        assert_eq!(opus_pt("m=audio 9 RTP/AVP 0\r\n"), None);
    }

    #[test]
    fn injects_missing_candidates_per_media() {
        let cands = vec![
            (
                0,
                "candidate:1 1 UDP 100 198.51.100.7 5000 typ relay".to_owned(),
            ),
            (
                1,
                "candidate:2 1 UDP 100 198.51.100.7 5002 typ relay".to_owned(),
            ),
        ];
        let out = inject_candidates(SDP, &cands);
        let audio = out.split("m=video").next().unwrap();
        assert!(audio.contains("a=candidate:1 1 UDP 100 198.51.100.7 5000 typ relay\r\n"));
        assert!(!audio.contains("5002"));
        assert!(out.ends_with("a=candidate:2 1 UDP 100 198.51.100.7 5002 typ relay\r\n"));
        // už obsažené se nezdvojí
        assert_eq!(inject_candidates(&out, &cands), out);
    }
}
