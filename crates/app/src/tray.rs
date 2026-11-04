//! Ikona v liště (StatusNotifierItem): počet nepřečtených, zobrazení okna, ukončení.

use std::fmt;

use iced::Subscription;
use iced::futures::SinkExt;
use ksni::menu::StandardItem;
use ksni::{Icon, MenuItem, ToolTip, TrayMethods};
use tokio::sync::mpsc::UnboundedSender;

use crate::app::Msg;

/// Co uživatel udělal s ikonou.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayEvent {
    Show,
    Quit,
}

pub struct TymyTray {
    unread: usize,
    tx: UnboundedSender<TrayEvent>,
}

impl TymyTray {
    fn send(&self, e: TrayEvent) {
        let _ = self.tx.send(e);
    }
}

impl ksni::Tray for TymyTray {
    fn id(&self) -> String {
        "tymy".into()
    }

    fn title(&self) -> String {
        "Tymy".into()
    }

    /// Vlastní ikona (bublina chatu), nezávislá na tématu ikon.
    fn icon_pixmap(&self) -> Vec<Icon> {
        [22, 32, 48, 64]
            .into_iter()
            .map(|size| bubble_icon(size, self.unread > 0))
            .collect()
    }

    fn tool_tip(&self) -> ToolTip {
        ToolTip {
            title: "Tymy".into(),
            description: match self.unread {
                0 => "Žádné nepřečtené zprávy".into(),
                1 => "1 nepřečtená konverzace".into(),
                n @ 2..=4 => format!("{n} nepřečtené konverzace"),
                n => format!("{n} nepřečtených konverzací"),
            },
            ..Default::default()
        }
    }

    /// Klik levým tlačítkem: zobrazit okno.
    fn activate(&mut self, _x: i32, _y: i32) {
        self.send(TrayEvent::Show);
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        vec![
            StandardItem {
                label: "Zobrazit Tymy".into(),
                activate: Box::new(|t: &mut Self| t.send(TrayEvent::Show)),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: "Ukončit".into(),
                icon_name: "application-exit".into(),
                activate: Box::new(|t: &mut Self| t.send(TrayEvent::Quit)),
                ..Default::default()
            }
            .into(),
        ]
    }
}

/// Ovladač běžící ikony.
#[derive(Clone)]
pub struct TrayHandle(ksni::Handle<TymyTray>);

impl fmt::Debug for TrayHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("TrayHandle")
    }
}

impl TrayHandle {
    pub async fn set_unread(self, n: usize) {
        self.0.update(|t| t.unread = n).await;
    }
}

/// Spustí ikonu a převádí její události na zprávy aplikace.
pub fn subscription() -> Subscription<Msg> {
    Subscription::run(|| {
        iced::stream::channel(16, async |mut out| {
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
            match (TymyTray { unread: 0, tx }).spawn().await {
                Ok(handle) => {
                    let _ = out.send(Msg::TrayReady(TrayHandle(handle))).await;
                }
                Err(e) => {
                    tracing::warn!(error = %e, "ikona v liště není k dispozici");
                    let _ = out.send(Msg::TrayUnavailable).await;
                    return;
                }
            }
            while let Some(event) = rx.recv().await {
                let _ = out.send(Msg::Tray(event)).await;
            }
        })
    })
}

/// Barva bubliny (tyrkysová, ať se neplete s žádnou známou aplikací).
const BUBBLE: [u8; 3] = [0x1E, 0x88, 0x9E];
const BADGE: [u8; 3] = [0xE5, 0x39, 0x35];

/// Ikona okna (stejná bublina jako v liště), RGBA.
pub fn window_icon(size: u32) -> Option<iced::window::Icon> {
    let argb = bubble_icon(size as i32, false).data;
    let rgba: Vec<u8> = argb
        .chunks_exact(4)
        .flat_map(|p| [p[1], p[2], p[3], p[0]])
        .collect();
    iced::window::icon::from_rgba(rgba, size, size).ok()
}

/// Nakreslí bublinu chatu se třemi tečkami; `unread` přidá červenou tečku vpravo nahoře.
/// Vyhlazení hran: 4×4 vzorky na pixel.
fn bubble_icon(size: i32, unread: bool) -> Icon {
    let n = size as f32;
    let mut data = Vec::with_capacity((size * size * 4) as usize);
    for py in 0..size {
        for px in 0..size {
            let (mut bubble, mut dots, mut badge) = (0u32, 0u32, 0u32);
            for sy in 0..4 {
                for sx in 0..4 {
                    let x = (px as f32 + (sx as f32 + 0.5) / 4.0) / n;
                    let y = (py as f32 + (sy as f32 + 0.5) / 4.0) / n;
                    let in_badge = unread && dist(x, y, 0.79, 0.21) < 0.19;
                    // průhledný okraj kolem odznaku, aby byl vidět i na bublině
                    let in_ring = unread && dist(x, y, 0.79, 0.21) < 0.25;
                    if in_badge {
                        badge += 1;
                    } else if !in_ring && in_bubble(x, y) {
                        bubble += 1;
                        if [0.30, 0.48, 0.66]
                            .iter()
                            .any(|cx| dist(x, y, *cx, 0.47) < 0.065)
                        {
                            dots += 1;
                        }
                    }
                }
            }
            let (a, rgb) = if badge > 0 {
                (badge, BADGE)
            } else if dots > 0 {
                // bílé tečky na bublině
                let t = dots as f32 / bubble.max(1) as f32;
                let mix = |c: u8| (c as f32 * (1.0 - t) + 255.0 * t) as u8;
                (bubble, [mix(BUBBLE[0]), mix(BUBBLE[1]), mix(BUBBLE[2])])
            } else {
                (bubble, BUBBLE)
            };
            data.extend([(a * 255 / 16) as u8, rgb[0], rgb[1], rgb[2]]);
        }
    }
    Icon {
        width: size,
        height: size,
        data,
    }
}

fn dist(x: f32, y: f32, cx: f32, cy: f32) -> f32 {
    ((x - cx).powi(2) + (y - cy).powi(2)).sqrt()
}

/// Zaoblený obdélník + „ocásek“ vlevo dole (souřadnice 0..1).
fn in_bubble(x: f32, y: f32) -> bool {
    let (l, t, r, b, rad) = (0.06, 0.14, 0.94, 0.78, 0.2);
    let cx = x.clamp(l + rad, r - rad);
    let cy = y.clamp(t + rad, b - rad);
    let in_rect = x >= l && x <= r && y >= t && y <= b && dist(x, y, cx, cy) <= rad;
    // ocásek: trojúhelník (0.2,0.74)–(0.42,0.74)–(0.16,0.95)
    let in_tail = {
        let (x1, y1, x2, y2, x3, y3) = (0.20, 0.74, 0.42, 0.74, 0.16, 0.95);
        let d = (y2 - y3) * (x1 - x3) + (x3 - x2) * (y1 - y3);
        let a = ((y2 - y3) * (x - x3) + (x3 - x2) * (y - y3)) / d;
        let bb = ((y3 - y1) * (x - x3) + (x1 - x3) * (y - y3)) / d;
        a >= 0.0 && bb >= 0.0 && a + bb <= 1.0
    };
    in_rect || in_tail
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alpha(icon: &Icon, x: i32, y: i32) -> u8 {
        icon.data[((y * icon.width + x) * 4) as usize]
    }

    #[test]
    fn icon_has_expected_shape() {
        let i = bubble_icon(32, false);
        assert_eq!(i.data.len(), 32 * 32 * 4);
        assert_eq!(alpha(&i, 0, 0), 0, "roh je průhledný");
        assert_eq!(alpha(&i, 16, 8), 255, "střed horní části bubliny je plný");
        // bez nepřečtených je pravý horní roh bubliny modrý, s nepřečtenými červený
        let at = |i: &Icon| {
            let o = ((7 * 32 + 25) * 4) as usize;
            i.data[o + 1..o + 4].to_vec()
        };
        assert_eq!(at(&i), BUBBLE);
        assert_eq!(at(&bubble_icon(32, true)), BADGE);
    }

    /// Ruční náhled: `TYMY_ICON_DUMP=<adresář> cargo test -p tymy dump_icon`.
    #[test]
    fn dump_icon() {
        let Ok(dir) = std::env::var("TYMY_ICON_DUMP") else {
            return;
        };
        for unread in [false, true] {
            let i = bubble_icon(64, unread);
            std::fs::write(format!("{dir}/icon-{unread}.argb"), &i.data).unwrap();
        }
    }
}
