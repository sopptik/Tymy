//! Dvě instance enginu spojené mezi sebou (bez sítě Teams): tón z jedné musí ve druhé
//! dorazit jako slyšitelný zvuk. Potřebuje pluginy GStreameru za běhu, proto `--ignored`.

use std::time::Duration;

use tymy_core::Media;
use tymy_media::{Config, GstMedia, Sink, Source};

fn engine() -> GstMedia {
    GstMedia::new(Config {
        turn: vec![],
        source: Source::TestTone,
        sink: Sink::Meter,
    })
    .expect("engine")
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "potřebuje GStreamer s webrtcbin, opus a libnice"]
async fn tone_goes_through() {
    let (a, b) = (engine(), engine());
    let offer = a.offer().await.expect("nabídka");
    assert!(
        offer.contains("a=candidate:"),
        "nabídka bez kandidátů:\n{offer}"
    );
    let answer = b.answer(&offer).await.expect("odpověď");
    a.accept_answer(&answer).await.expect("přijetí odpovědi");
    tokio::time::sleep(Duration::from_secs(4)).await;
    let (sa, sb) = (a.stats().await, b.stats().await);
    eprintln!("a: {sa:?}\nb: {sb:?}");
    assert!(sa.packets_sent > 0 && sb.packets_received > 0);
    assert!(sb.loud_samples > 0 && sa.loud_samples > 0, "zvuk nedorazil");
}

/// Odpověď ve tvaru, jaký vrací mediální server Teams po překladu `tymy_api::sdp::from_ms`
/// (smyšlené adresy a klíče). Bez `a=sendrecv` ji `webrtcbin` odmítne.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "potřebuje GStreamer s webrtcbin, opus a libnice"]
async fn accepts_server_style_answer() {
    let a = engine();
    let offer = a.offer().await.expect("nabídka");
    let fp = offer
        .lines()
        .find(|l| l.starts_with("a=fingerprint:"))
        .expect("otisk")
        .to_owned();
    let answer = format!(
        "v=0\r\no=- 486708 0 IN IP4 198.51.100.7\r\ns=session\r\nc=IN IP4 198.51.100.7\r\n\
b=CT:10000000\r\nt=0 0\r\na=group:BUNDLE audio0\r\n\
m=audio 3480 UDP/TLS/RTP/SAVPF 111\r\nc=IN IP4 198.51.100.7\r\na=rtcp-rsize\r\n\
a=rtcp:3480\r\na=setup:passive\r\na=ice-ufrag:abcd\r\na=ice-pwd:abcdefghijklmnopqrstuvwx\r\n\
a=rtcp-mux\r\na=candidate:1 1 udp 54001663 198.51.100.7 3480 typ relay raddr 198.51.100.7 rport 3480\r\n\
a=mid:audio0\r\na=sendrecv\r\na=rtpmap:111 OPUS/48000/2\r\na=ptime:20\r\n{fp}\r\n"
    );
    a.accept_answer(&answer).await.expect("odpověď serveru");
}
