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
    /// Klik na připomínku schůzky v menu (pořadí v seznamu z [`TrayHandle::set_state`]).
    OpenMeeting(usize),
}

pub struct TymyTray {
    unread: usize,
    /// Připomenuté schůzky, které si uživatel ještě neotevřel (popisky).
    meetings: Vec<String>,
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
            .map(|size| bubble_icon(size, self.unread > 0, !self.meetings.is_empty()))
            .collect()
    }

    fn tool_tip(&self) -> ToolTip {
        ToolTip {
            title: "Tymy".into(),
            description: tooltip_text(self.unread, &self.meetings),
            ..Default::default()
        }
    }

    /// Klik levým tlačítkem: zobrazit okno.
    fn activate(&mut self, _x: i32, _y: i32) {
        self.send(TrayEvent::Show);
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let mut items: Vec<MenuItem<Self>> = self
            .meetings
            .iter()
            .enumerate()
            .map(|(i, label)| {
                StandardItem {
                    label: format!("Schůzka: {label}"),
                    icon_name: "appointment-soon".into(),
                    activate: Box::new(move |t: &mut Self| t.send(TrayEvent::OpenMeeting(i))),
                    ..Default::default()
                }
                .into()
            })
            .collect();
        if !items.is_empty() {
            items.push(MenuItem::Separator);
        }
        items.extend([
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
        ]);
        items
    }
}

/// Popisek ikony: připomenuté schůzky (odlišené od chatů) a počet nepřečtených konverzací.
fn tooltip_text(unread: usize, meetings: &[String]) -> String {
    let mut lines: Vec<String> = match meetings {
        [] => vec![],
        [one] => vec![format!("Připomínka schůzky: {one}")],
        many => std::iter::once(format!("Připomínky schůzek ({}):", many.len()))
            .chain(many.iter().map(|m| format!("• {m}")))
            .collect(),
    };
    lines.push(match unread {
        0 => "Žádné nepřečtené zprávy".into(),
        1 => "1 nepřečtená konverzace".into(),
        n @ 2..=4 => format!("{n} nepřečtené konverzace"),
        n => format!("{n} nepřečtených konverzací"),
    });
    lines.join("\n")
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
    /// Počet nepřečtených konverzací a popisky připomenutých schůzek.
    pub async fn set_state(self, unread: usize, meetings: Vec<String>) {
        self.0
            .update(|t| {
                t.unread = unread;
                t.meetings = meetings;
            })
            .await;
    }
}

/// Spustí ikonu a převádí její události na zprávy aplikace.
pub fn subscription() -> Subscription<Msg> {
    Subscription::run(|| {
        iced::stream::channel(16, async |mut out| {
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
            let tray = TymyTray {
                unread: 0,
                meetings: vec![],
                tx,
            };
            match tray.spawn().await {
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
/// Odznak připomenuté schůzky (oranžový s ručičkami hodin), vpravo dole.
const MEETING: [u8; 3] = [0xF5, 0x9E, 0x0B];
const MEETING_AT: (f32, f32) = (0.79, 0.79);

/// Ikona okna (stejná bublina jako v liště), RGBA.
pub fn window_icon(size: u32) -> Option<iced::window::Icon> {
    let argb = bubble_icon(size as i32, false, false).data;
    let rgba: Vec<u8> = argb
        .as_chunks::<4>()
        .0
        .iter()
        .flat_map(|[a, r, g, b]| [*r, *g, *b, *a])
        .collect();
    iced::window::icon::from_rgba(rgba, size, size).ok()
}

/// Nakreslí bublinu chatu se třemi tečkami; `unread` přidá červenou tečku vpravo nahoře,
/// `meeting` oranžové hodiny vpravo dole. Vyhlazení hran: 4×4 vzorky na pixel.
fn bubble_icon(size: i32, unread: bool, meeting: bool) -> Icon {
    let n = size as f32;
    let mut data = Vec::with_capacity((size * size * 4) as usize);
    for py in 0..size {
        for px in 0..size {
            let (mut bubble, mut dots, mut badge) = (0u32, 0u32, 0u32);
            let (mut clock, mut hands) = (0u32, 0u32);
            for sy in 0..4 {
                for sx in 0..4 {
                    let x = (px as f32 + (sx as f32 + 0.5) / 4.0) / n;
                    let y = (py as f32 + (sy as f32 + 0.5) / 4.0) / n;
                    let in_badge = unread && dist(x, y, 0.79, 0.21) < 0.19;
                    let (mx, my) = MEETING_AT;
                    let in_clock = meeting && dist(x, y, mx, my) < 0.2;
                    // průhledný okraj kolem odznaků, aby byly vidět i na bublině
                    let in_ring = (unread && dist(x, y, 0.79, 0.21) < 0.25)
                        || (meeting && dist(x, y, mx, my) < 0.26);
                    if in_badge {
                        badge += 1;
                    } else if in_clock {
                        clock += 1;
                        // ručičky: nahoru a doprava ze středu
                        let up = (x - mx).abs() < 0.035 && y <= my + 0.02 && y > my - 0.13;
                        let right = (y - my).abs() < 0.035 && x >= mx - 0.02 && x < mx + 0.1;
                        if up || right {
                            hands += 1;
                        }
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
            } else if clock > 0 {
                let t = hands as f32 / clock as f32;
                let mix = |c: u8| (c as f32 * (1.0 - t) + 255.0 * t) as u8;
                (clock, [mix(MEETING[0]), mix(MEETING[1]), mix(MEETING[2])])
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
        let i = bubble_icon(32, false, false);
        assert_eq!(i.data.len(), 32 * 32 * 4);
        assert_eq!(alpha(&i, 0, 0), 0, "roh je průhledný");
        assert_eq!(alpha(&i, 16, 8), 255, "střed horní části bubliny je plný");
        // bez nepřečtených je pravý horní roh bubliny modrý, s nepřečtenými červený
        let at = |i: &Icon| {
            let o = ((7 * 32 + 25) * 4) as usize;
            i.data[o + 1..o + 4].to_vec()
        };
        assert_eq!(at(&i), BUBBLE);
        assert_eq!(at(&bubble_icon(32, true, false)), BADGE);
        // schůzka: oranžový odznak vpravo dole (mimo ručičky), nepřečtené zůstávají
        let m = bubble_icon(32, true, true);
        let px = |i: &Icon, x: usize, y: usize| {
            let o = (y * 32 + x) * 4;
            i.data[o..o + 4].to_vec()
        };
        assert_eq!(
            px(&m, 22, 28),
            vec![255, MEETING[0], MEETING[1], MEETING[2]]
        );
        assert_eq!(at(&m), BADGE);
        assert_eq!(alpha(&i, 30, 30), 0, "bez schůzky je roh prázdný");
    }

    #[test]
    fn tooltip_tells_meetings_from_chats() {
        assert_eq!(tooltip_text(0, &[]), "Žádné nepřečtené zprávy");
        assert_eq!(
            tooltip_text(2, &["Porada v 10:00".into()]),
            "Připomínka schůzky: Porada v 10:00\n2 nepřečtené konverzace"
        );
        let t = tooltip_text(0, &["A".into(), "B".into()]);
        assert!(t.starts_with("Připomínky schůzek (2):\n• A\n• B"));
    }

    /// Ruční náhled: `TYMY_ICON_DUMP=<adresář> cargo test -p tymy dump_icon`.
    #[test]
    fn dump_icon() {
        let Ok(dir) = std::env::var("TYMY_ICON_DUMP") else {
            return;
        };
        for (unread, meeting) in [(false, false), (true, false), (true, true), (false, true)] {
            let i = bubble_icon(64, unread, meeting);
            std::fs::write(format!("{dir}/icon-{unread}-{meeting}.argb"), &i.data).unwrap();
        }
    }
}
