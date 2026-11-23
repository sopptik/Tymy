//! Dvě instance enginu spojené mezi sebou (bez sítě Teams): tón z jedné musí ve druhé
//! dorazit jako slyšitelný zvuk. Potřebuje pluginy GStreameru za běhu, proto `--ignored`.

use std::time::Duration;

use tymy_core::Media;
use tymy_media::{Config, GstMedia, Sink, Source, VideoSource};

fn engine() -> GstMedia {
    engine_with(None)
}

fn engine_with(video: Option<VideoSource>) -> GstMedia {
    GstMedia::new(Config {
        turn: vec![],
        source: Source::TestTone,
        sink: Sink::Meter,
        video,
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

/// Videohovor: testovací obrazec z jedné instance se ve druhé dekóduje na snímky
/// (a naopak), náhled vlastní „kamery“ běží; vypnutí kamery náhled zastaví.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "potřebuje GStreamer s webrtcbin, opus, openh264 a libnice"]
async fn video_goes_through() {
    let (a, b) = (
        engine_with(Some(VideoSource::TestPattern)),
        engine_with(Some(VideoSource::TestPattern)),
    );
    let offer = a.offer().await.expect("nabídka");
    assert!(offer.contains("m=video"), "nabídka bez obrazu:\n{offer}");
    let answer = b.answer(&offer).await.expect("odpověď");
    assert!(answer.contains("m=video"), "odpověď bez obrazu:\n{answer}");
    a.accept_answer(&answer).await.expect("přijetí odpovědi");
    tokio::time::sleep(Duration::from_secs(5)).await;
    let (sa, sb) = (a.stats().await, b.stats().await);
    eprintln!("a: {sa:?}\nb: {sb:?}");
    let frame = b.remote_frame().expect("snímek protistrany");
    assert_eq!((frame.width, frame.height), (640, 360));
    assert_eq!(frame.rgba.len(), 640 * 360 * 4);
    assert!(
        sb.video_frames > 10 && sa.video_frames > 10,
        "obraz nedorazil"
    );
    assert!(sb.loud_samples > 0, "zvuk vedle obrazu nedorazil");
    assert!(a.local_frame().is_some() && a.camera_on());
    a.set_camera(false).expect("vypnutí kamery");
    assert!(a.local_frame().is_none() && !a.camera_on());
    a.set_camera(true).expect("zapnutí kamery");
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert!(a.local_frame().is_some(), "náhled po zapnutí kamery");
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

/// Videohovor s protistranou, která obraz odmítne (echo bot): po překladu
/// `tymy_api::sdp::from_ms` musí `webrtcbin` odpověď přijmout, aby šel aspoň zvuk.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "potřebuje GStreamer s webrtcbin, opus, openh264 a libnice"]
async fn accepts_answer_with_rejected_video() {
    let a = engine_with(Some(VideoSource::TestPattern));
    let offer = a.offer().await.expect("nabídka");
    let fp = offer
        .lines()
        .find(|l| l.starts_with("a=fingerprint:"))
        .expect("otisk")
        .to_owned();
    let mids: Vec<&str> = offer
        .lines()
        .filter_map(|l| l.strip_prefix("a=mid:"))
        .collect();
    let answer = format!(
        "v=0\r\no=- 486708 0 IN IP4 198.51.100.7\r\ns=session\r\n\
b=CT:10000000\r\nt=0 0\r\na=group:BUNDLE {a0} {v1}\r\na=x-plaza-msi-range:1-100 101-200\r\n\
m=audio 3480 RTP/SAVP 111\r\nc=IN IP4 198.51.100.7\r\na=rtcp-rsize\r\n\
a=x-source-streamid:201\r\na=rtcp:3480\r\na=setup:passive\r\na=ice-ufrag:abcd\r\n\
a=ice-pwd:abcdefghijklmnopqrstuvwx\r\na=rtcp-mux\r\n\
a=candidate:1 1 UDP 54001663 198.51.100.7 3480 typ relay raddr 198.51.100.7 rport 3480\r\n\
a=label:main-audio\r\na=mid:{a0}\r\na=rtpmap:111 OPUS/48000/2\r\na=ptime:20\r\n{fp}\r\n\
m=video 0 RTP/AVP 107\r\na=label:main-video\r\n",
        a0 = mids[0],
        v1 = mids[1],
    );
    let answer = tymy_api::sdp::from_ms(&answer);
    a.accept_answer(&answer)
        .await
        .unwrap_or_else(|e| panic!("{e}\n{answer}"));
}
