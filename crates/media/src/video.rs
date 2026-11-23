//! Obraz hovoru: kamera ve vlastní pipeline (vypnutí kamery ji zastaví celou, takže zhasne
//! i kontrolka), kódování H264 do `webrtcbin` a dekódování obrazu protistrany do snímků
//! RGBA pro okno.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use gst::prelude::*;
use tymy_core::{Error, Result};

use crate::err;

/// Rozlišení a snímková frekvence odesílaného obrazu (360p stačí pro 1:1 hovor a je
/// v mezích, které Teams nabízí i pro slabší linky).
pub const WIDTH: i32 = 640;
pub const HEIGHT: i32 = 360;
pub const FPS: i32 = 15;
/// Datový tok kodéru (bit/s).
const BITRATE: u32 = 600_000;

/// Odkud bere hovor obraz.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoSource {
    /// Výchozí kamera (V4L2).
    Camera,
    /// Testovací obrazec (zkouška bez kamery).
    TestPattern,
}

/// Jeden snímek RGBA (řádky bez výplně).
#[derive(Clone, PartialEq, Eq)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    /// Pořadí snímku; okno podle něj pozná, že je nový.
    pub seq: u64,
}

impl std::fmt::Debug for Frame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Frame({}×{}, #{})", self.width, self.height, self.seq)
    }
}

/// Poslední snímek jednoho proudu a počet všech snímků.
#[derive(Default)]
pub struct Slot {
    frame: Mutex<Option<Arc<Frame>>>,
    count: AtomicU64,
}

impl Slot {
    pub fn latest(&self) -> Option<Arc<Frame>> {
        self.frame.lock().expect("zámek").clone()
    }

    pub fn count(&self) -> u64 {
        self.count.load(Ordering::Relaxed)
    }

    fn clear(&self) {
        *self.frame.lock().expect("zámek") = None;
    }

    /// Uloží snímek z `appsink` (formát RGBA podle caps).
    fn store(&self, sample: &gst::Sample) {
        let (Some(buffer), Some(caps)) = (sample.buffer(), sample.caps()) else {
            return;
        };
        let Ok(info) = gst_video::VideoInfo::from_caps(caps) else {
            return;
        };
        let Ok(frame) = gst_video::VideoFrameRef::from_buffer_ref_readable(buffer, &info) else {
            return;
        };
        let (w, h) = (info.width() as usize, info.height() as usize);
        let stride = info.stride()[0] as usize;
        let Ok(data) = frame.plane_data(0) else {
            return;
        };
        let mut rgba = Vec::with_capacity(w * h * 4);
        for row in data.chunks(stride).take(h) {
            rgba.extend_from_slice(&row[..w * 4]);
        }
        let seq = self.count.fetch_add(1, Ordering::Relaxed) + 1;
        *self.frame.lock().expect("zámek") = Some(Arc::new(Frame {
            width: w as u32,
            height: h as u32,
            rgba,
            seq,
        }));
    }
}

fn make(factory: &str) -> Result<gst::Element> {
    gst::ElementFactory::make(factory)
        .build()
        .map_err(|e| err(factory, e))
}

fn raw_caps(format: &str) -> gst::Caps {
    gst::Caps::builder("video/x-raw")
        .field("format", format)
        .field("width", WIDTH)
        .field("height", HEIGHT)
        .field("framerate", gst::Fraction::new(FPS, 1))
        .build()
}

fn capsfilter(caps: &gst::Caps) -> Result<gst::Element> {
    gst::ElementFactory::make("capsfilter")
        .property("caps", caps)
        .build()
        .map_err(|e| err("capsfilter", e))
}

/// `appsink`, který drží jen nejnovější snímek a každý předá do `slot`.
fn frame_sink(slot: Arc<Slot>) -> gst::Element {
    let sink = gst_app::AppSink::builder()
        .caps(
            &gst::Caps::builder("video/x-raw")
                .field("format", "RGBA")
                .build(),
        )
        .max_buffers(1)
        .drop(true)
        .sync(false)
        .callbacks(
            gst_app::AppSinkCallbacks::builder()
                .new_sample(move |sink| {
                    if let Ok(sample) = sink.pull_sample() {
                        slot.store(&sample);
                    }
                    Ok(gst::FlowSuccess::Ok)
                })
                .build(),
        )
        .build();
    sink.upcast()
}

/// Kamera ve vlastní pipeline: snímky I420 pro kodér (`appsrc` v pipeline hovoru)
/// a RGBA pro náhled ve vlastním okně.
pub struct Camera {
    pipeline: gst::Pipeline,
}

impl Camera {
    pub fn start(
        source: VideoSource,
        encoder: gst_app::AppSrc,
        preview: Arc<Slot>,
    ) -> Result<Self> {
        let pipeline = gst::Pipeline::with_name("kamera");
        let src = match source {
            VideoSource::Camera => make("v4l2src")?,
            VideoSource::TestPattern => gst::ElementFactory::make("videotestsrc")
                .property("is-live", true)
                .property_from_str("pattern", "ball")
                .build()
                .map_err(|e| err("videotestsrc", e))?,
        };
        let convert = make("videoconvert")?;
        let scale = make("videoscale")?;
        let rate = make("videorate")?;
        let caps = capsfilter(&raw_caps("I420"))?;
        let tee = make("tee")?;
        let q_enc = make("queue")?;
        let to_enc = gst_app::AppSink::builder()
            .caps(&raw_caps("I420"))
            .max_buffers(2)
            .drop(true)
            .sync(false)
            .callbacks(
                gst_app::AppSinkCallbacks::builder()
                    .new_sample(move |sink| {
                        if let Ok(sample) = sink.pull_sample()
                            && let Some(buffer) = sample.buffer_owned()
                        {
                            // časové značky dá `appsrc` (do-timestamp), kamerové se zahodí
                            let mut buffer = buffer;
                            {
                                let b = buffer.make_mut();
                                b.set_pts(gst::ClockTime::NONE);
                                b.set_dts(gst::ClockTime::NONE);
                            }
                            let _ = encoder.push_buffer(buffer);
                        }
                        Ok(gst::FlowSuccess::Ok)
                    })
                    .build(),
            )
            .build();
        let q_prev = make("queue")?;
        let prev_convert = make("videoconvert")?;
        let prev = frame_sink(preview);
        let to_enc_el: &gst::Element = to_enc.upcast_ref();
        pipeline
            .add_many([
                &src,
                &convert,
                &scale,
                &rate,
                &caps,
                &tee,
                &q_enc,
                to_enc_el,
                &q_prev,
                &prev_convert,
                &prev,
            ])
            .map_err(|e| err("pipeline kamery", e))?;
        gst::Element::link_many([&src, &convert, &scale, &rate, &caps, &tee])
            .map_err(|e| err("propojení kamery", e))?;
        gst::Element::link_many([&tee, &q_enc, to_enc_el]).map_err(|e| err("kamera → kodér", e))?;
        gst::Element::link_many([&tee, &q_prev, &prev_convert, &prev])
            .map_err(|e| err("kamera → náhled", e))?;
        pipeline
            .set_state(gst::State::Playing)
            .map_err(|e| err("spuštění kamery (je připojená a volná?)", e))?;
        Ok(Self { pipeline })
    }
}

impl Drop for Camera {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

/// Vysílací větev obrazu v pipeline hovoru: `appsrc` → H264 → RTP. Vrací `appsrc`
/// (krmí ho [`Camera`]) a poslední prvek, který se napojí na `webrtcbin`.
pub fn sender(
    pipeline: &gst::Pipeline,
    pt: u32,
    ssrc: u32,
) -> Result<(gst_app::AppSrc, gst::Element)> {
    let src = gst_app::AppSrc::builder()
        .caps(&raw_caps("I420"))
        .is_live(true)
        .do_timestamp(true)
        .format(gst::Format::Time)
        .max_bytes(2 * (WIDTH * HEIGHT * 3 / 2) as u64)
        .build();
    let queue = make("queue")?;
    let enc = h264_encoder()?;
    let enc_caps = capsfilter(
        &gst::Caps::builder("video/x-h264")
            .field("profile", "constrained-baseline")
            .build(),
    )?;
    let parse = make("h264parse")?;
    let pay = gst::ElementFactory::make("rtph264pay")
        .property("pt", pt)
        .property("ssrc", ssrc)
        .property("config-interval", -1i32)
        .property_from_str("aggregate-mode", "zero-latency")
        .build()
        .map_err(|e| err("rtph264pay", e))?;
    let rtp_caps = capsfilter(
        &gst::Caps::builder("application/x-rtp")
            .field("media", "video")
            .field("encoding-name", "H264")
            .field("clock-rate", 90000i32)
            .field("payload", pt as i32)
            .field("ssrc", ssrc)
            .build(),
    )?;
    let src_el: &gst::Element = src.upcast_ref();
    let chain = [src_el, &queue, &enc, &enc_caps, &parse, &pay, &rtp_caps];
    pipeline.add_many(chain).map_err(|e| err("pipeline", e))?;
    gst::Element::link_many(chain).map_err(|e| err("propojení obrazu", e))?;
    for e in chain {
        e.sync_state_with_parent()
            .map_err(|e| err("spuštění obrazu", e))?;
    }
    Ok((src, rtp_caps))
}

/// Kodér H264: openh264 (v `gstreamer1.0-plugins-bad`), jinak x264 (`plugins-ugly`).
fn h264_encoder() -> Result<gst::Element> {
    if let Ok(e) = gst::ElementFactory::make("openh264enc")
        .property("bitrate", BITRATE)
        .property_from_str("rate-control", "bitrate")
        .property("gop-size", (FPS * 4) as u32)
        .property_from_str("usage-type", "camera")
        .build()
    {
        return Ok(e);
    }
    gst::ElementFactory::make("x264enc")
        .property_from_str("tune", "zerolatency")
        .property_from_str("speed-preset", "ultrafast")
        .property("bitrate", BITRATE / 1000)
        .property("key-int-max", (FPS * 4) as u32)
        .build()
        .map_err(|_| Error::Other("chybí kodér H264 (openh264enc ani x264enc)".into()))
}

/// Dekódovaný obraz protistrany → snímky RGBA do `slot`.
pub fn output(pipeline: &gst::Pipeline, pad: &gst::Pad, slot: Arc<Slot>) -> Result<()> {
    let queue = gst::ElementFactory::make("queue")
        .property_from_str("leaky", "downstream")
        .property("max-size-buffers", 2u32)
        .build()
        .map_err(|e| err("queue", e))?;
    let convert = make("videoconvert")?;
    let sink = frame_sink(slot);
    let chain = [&queue, &convert, &sink];
    pipeline.add_many(chain).map_err(|e| err("pipeline", e))?;
    gst::Element::link_many(chain).map_err(|e| err("propojení obrazu", e))?;
    for e in chain {
        e.sync_state_with_parent()
            .map_err(|e| err("spuštění obrazu", e))?;
    }
    pad.link(&queue.static_pad("sink").expect("queue má vstup"))
        .map_err(|e| err("napojení obrazu", e))?;
    Ok(())
}

/// Pozdější vypnutí kamery smaže náhled, aby okno neukazovalo zamrzlý snímek.
pub fn clear(slot: &Slot) {
    slot.clear();
}

/// Typ obsahu H264 z nabídky protistrany (přednost má `packetization-mode=1`).
pub fn h264_pt(sdp: &str) -> Option<u32> {
    let pts: Vec<u32> = sdp
        .lines()
        .filter_map(|l| {
            let (pt, codec) = l.strip_prefix("a=rtpmap:")?.split_once(' ')?;
            codec
                .to_ascii_lowercase()
                .starts_with("h264/")
                .then(|| pt.parse().ok())
                .flatten()
        })
        .collect();
    let mode1 = |pt: &u32| {
        sdp.lines().any(|l| {
            l.strip_prefix(&format!("a=fmtp:{pt} "))
                .is_some_and(|f| f.contains("packetization-mode=1"))
        })
    };
    pts.iter().copied().find(mode1).or(pts.first().copied())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefers_h264_with_packetization_mode_1() {
        let sdp = "m=video 9 UDP/TLS/RTP/SAVPF 100 107\r\na=rtpmap:100 H264/90000\r\n\
a=fmtp:100 packetization-mode=0\r\na=rtpmap:107 H264/90000\r\n\
a=fmtp:107 profile-level-id=42e01f;packetization-mode=1\r\n";
        assert_eq!(h264_pt(sdp), Some(107));
        assert_eq!(h264_pt("a=rtpmap:96 VP8/90000\r\n"), None);
    }
}
