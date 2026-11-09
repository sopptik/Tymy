//! Vzhled aplikace: palety pro světlý/tmavý režim, styly widgetů a drobné sdílené prvky.
//!
//! Všechny styly berou barvy z [`colors`] podle toho, zda je aktivní téma tmavé,
//! takže přepnutí režimu je jen výměna [`Theme`].

use iced::border::Radius;
use iced::widget::{button, container, scrollable, text, text_editor, text_input};
use iced::{Background, Border, Color, Font, Shadow, Theme, Vector, color, font};

pub const BOLD: Font = Font {
    weight: font::Weight::Bold,
    ..Font::DEFAULT
};
/// Barevná emoji (Noto Color Emoji). Bez něj by nouzový výběr písma vzal u části emoji
/// černobílé glyfy z jiného fontu; když font chybí, iced použije běžný výběr.
pub const EMOJI: Font = Font::with_name("Noto Color Emoji");

pub const SEMIBOLD: Font = Font {
    weight: font::Weight::Semibold,
    ..Font::DEFAULT
};

/// Barevná sada jednoho režimu.
#[derive(Debug, Clone, Copy)]
pub struct Colors {
    /// Pozadí vlákna zpráv.
    pub app: Color,
    /// Pozadí levého panelu a hlaviček.
    pub sidebar: Color,
    /// Karty, bubliny ostatních, pole.
    pub surface: Color,
    /// Najetí myší, vstupní pole, segmentový přepínač.
    pub surface_alt: Color,
    pub border: Color,
    pub text: Color,
    pub muted: Color,
    pub primary: Color,
    pub primary_hover: Color,
    pub on_primary: Color,
    /// Jemné zvýraznění vybrané položky.
    pub primary_soft: Color,
    pub danger: Color,
    pub danger_soft: Color,
    pub shadow: Color,
    /// Vlastní bubliny: pozadí, text a odkazy (v tmavém vzhledu tlumené, ne syté `primary`).
    pub mine: Color,
    pub on_mine: Color,
    pub mine_link: Color,
}

const LIGHT: Colors = Colors {
    app: color!(0xF3F5F7),
    sidebar: color!(0xFFFFFF),
    surface: color!(0xFFFFFF),
    surface_alt: color!(0xEDF0F3),
    border: color!(0xE1E5EA),
    text: color!(0x1B1F24),
    muted: color!(0x6A7380),
    primary: color!(0x13879D),
    primary_hover: color!(0x0F7385),
    on_primary: color!(0xFFFFFF),
    primary_soft: color!(0xDDF1F5),
    danger: color!(0xD93636),
    danger_soft: color!(0xFDE7E7),
    shadow: Color::from_rgba(0.06, 0.09, 0.13, 0.10),
    mine: color!(0x13879D),
    on_mine: color!(0xFFFFFF),
    mine_link: color!(0xFFFFFF),
};

const DARK: Colors = Colors {
    app: color!(0x111417),
    sidebar: color!(0x171A1E),
    surface: color!(0x20242A),
    surface_alt: color!(0x282D34),
    border: color!(0x2C3138),
    text: color!(0xE6E9ED),
    muted: color!(0x8C949E),
    primary: color!(0x1D97AD),
    primary_hover: color!(0x27A9C0),
    on_primary: color!(0xFFFFFF),
    primary_soft: color!(0x173A42),
    danger: color!(0xF06060),
    danger_soft: color!(0x3A1F22),
    shadow: Color::from_rgba(0.0, 0.0, 0.0, 0.35),
    mine: color!(0x14373E),
    on_mine: color!(0xE8F3F5),
    mine_link: color!(0x8EDBEA),
};

pub fn colors(t: &Theme) -> Colors {
    if t.extended_palette().is_dark {
        DARK
    } else {
        LIGHT
    }
}

/// Téma iced pro daný režim (výchozí barvy widgetů, které nestylujeme sami).
pub fn theme(dark: bool) -> Theme {
    let c = if dark { DARK } else { LIGHT };
    Theme::custom(
        if dark { "Tymy tmavý" } else { "Tymy světlý" },
        iced::theme::Palette {
            background: c.app,
            text: c.text,
            primary: c.primary,
            success: color!(0x2E9E5B),
            warning: color!(0xD9A03A),
            danger: c.danger,
        },
    )
}

fn rounded(r: f32) -> Border {
    Border {
        radius: r.into(),
        ..Default::default()
    }
}

// ---------- plochy ----------

pub fn sidebar(t: &Theme) -> container::Style {
    let c = colors(t);
    container::Style {
        background: Some(c.sidebar.into()),
        text_color: Some(c.text),
        border: Border {
            color: c.border,
            width: 0.0,
            ..Default::default()
        },
        ..Default::default()
    }
}

pub fn app_background(t: &Theme) -> container::Style {
    let c = colors(t);
    container::Style {
        background: Some(c.app.into()),
        text_color: Some(c.text),
        ..Default::default()
    }
}

/// Hlavička chatu: barva panelu a spodní linka.
pub fn header(t: &Theme) -> container::Style {
    let c = colors(t);
    container::Style {
        background: Some(c.sidebar.into()),
        text_color: Some(c.text),
        ..Default::default()
    }
}

pub fn divider(t: &Theme) -> container::Style {
    container::Style {
        background: Some(colors(t).border.into()),
        ..Default::default()
    }
}

/// Karta (přihlášení) se stínem.
pub fn card(t: &Theme) -> container::Style {
    let c = colors(t);
    container::Style {
        background: Some(c.surface.into()),
        text_color: Some(c.text),
        border: Border {
            radius: 16.0.into(),
            width: 1.0,
            color: c.border,
        },
        shadow: Shadow {
            color: c.shadow,
            offset: Vector::new(0.0, 8.0),
            blur_radius: 24.0,
        },
        ..Default::default()
    }
}

/// Psací pole: zaoblený rámeček kolem editoru a tlačítka.
pub fn composer(t: &Theme) -> container::Style {
    let c = colors(t);
    container::Style {
        background: Some(c.surface.into()),
        text_color: Some(c.text),
        border: Border {
            radius: 16.0.into(),
            width: 1.0,
            color: c.border,
        },
        shadow: Shadow {
            color: c.shadow,
            offset: Vector::new(0.0, 2.0),
            blur_radius: 8.0,
        },
        ..Default::default()
    }
}

pub fn error_banner(t: &Theme) -> container::Style {
    let c = colors(t);
    container::Style {
        background: Some(c.danger_soft.into()),
        text_color: Some(c.danger),
        ..Default::default()
    }
}

/// Štítek dne ve vláknu.
pub fn day_pill(t: &Theme) -> container::Style {
    let c = colors(t);
    container::Style {
        background: Some(c.surface_alt.into()),
        text_color: Some(c.muted),
        border: rounded(10.0),
        ..Default::default()
    }
}

/// Segmentový přepínač záložek (pozadí).
pub fn segmented(t: &Theme) -> container::Style {
    container::Style {
        background: Some(colors(t).surface_alt.into()),
        border: rounded(10.0),
        ..Default::default()
    }
}

pub fn tooltip(t: &Theme) -> container::Style {
    let c = colors(t);
    container::Style {
        background: Some(c.text.into()),
        text_color: Some(c.sidebar),
        border: rounded(6.0),
        ..Default::default()
    }
}

// ---------- bubliny a čipy ----------

/// Bublina zprávy. První ve skupině má „ocásek“ (menší zaoblení u odesílatele).
pub fn bubble(t: &Theme, mine: bool, first: bool) -> container::Style {
    let c = colors(t);
    let (r, tail) = (16.0, if first { 4.0 } else { 16.0 });
    let radius = if mine {
        Radius {
            top_left: r,
            top_right: tail,
            bottom_right: r,
            bottom_left: r,
        }
    } else {
        Radius {
            top_left: tail,
            top_right: r,
            bottom_right: r,
            bottom_left: r,
        }
    };
    let (bg, fg, border) = if mine {
        (c.mine, c.on_mine, Color::TRANSPARENT)
    } else {
        (c.surface, c.text, c.border)
    };
    container::Style {
        background: Some(bg.into()),
        text_color: Some(fg),
        border: Border {
            radius,
            width: if mine { 0.0 } else { 1.0 },
            color: border,
        },
        shadow: Shadow {
            color: Color {
                a: c.shadow.a * 0.5,
                ..c.shadow
            },
            offset: Vector::new(0.0, 1.0),
            blur_radius: 2.0,
        },
        ..Default::default()
    }
}

// ---------- obsah zpráv ----------

/// Barva textu v bublině (na vlastní bublině bílá, jinak běžná).
pub fn bubble_text(c: &Colors, mine: bool) -> Color {
    if mine { c.on_mine } else { c.text }
}

/// Jemný podklad uvnitř bubliny (kód, citace, tabulky) odvozený z barvy textu.
pub fn bubble_tint(c: &Colors, mine: bool, alpha: f32) -> Color {
    Color {
        a: alpha,
        ..bubble_text(c, mine)
    }
}

/// Barva odkazů a zmínek v bublině.
pub fn bubble_link(c: &Colors, mine: bool) -> Color {
    if mine { c.mine_link } else { c.primary }
}

/// Blok kódu.
pub fn code_block(t: &Theme, mine: bool) -> container::Style {
    let c = colors(t);
    container::Style {
        background: Some(bubble_tint(&c, mine, 0.09).into()),
        border: rounded(8.0),
        ..Default::default()
    }
}

/// Smíchá dvě barvy (`t` = podíl `b`), výsledek je neprůhledný.
pub fn mix(a: Color, b: Color, t: f32) -> Color {
    Color {
        r: a.r + (b.r - a.r) * t,
        g: a.g + (b.g - a.g) * t,
        b: a.b + (b.b - a.b) * t,
        a: 1.0,
    }
}

/// Pozadí bubliny (pro neprůhledné podklady uvnitř).
fn bubble_background(c: &Colors, mine: bool) -> Color {
    if mine { c.mine } else { c.surface }
}

/// Barva proužku u citace.
fn quote_bar_color(c: &Colors, mine: bool) -> Color {
    if mine {
        mix(c.mine, c.mine_link, 0.7)
    } else {
        c.primary
    }
}

/// Citace: vnější obal v barvě proužku; vnitřek ([`quote`]) ho zakryje až na 3 px vlevo.
pub fn quote_frame(t: &Theme, mine: bool) -> container::Style {
    let c = colors(t);
    container::Style {
        background: Some(quote_bar_color(&c, mine).into()),
        border: rounded(8.0),
        ..Default::default()
    }
}

/// Vnitřek citace (neprůhledný, aby proužek nebyl vidět skrz).
pub fn quote(t: &Theme, mine: bool) -> container::Style {
    let c = colors(t);
    container::Style {
        background: Some(mix(bubble_background(&c, mine), bubble_text(&c, mine), 0.07).into()),
        border: Border {
            radius: Radius {
                top_left: 2.0,
                top_right: 8.0,
                bottom_right: 8.0,
                bottom_left: 2.0,
            },
            ..Default::default()
        },
        ..Default::default()
    }
}

/// Vodorovná čára ve zprávě.
pub fn rule(t: &Theme, mine: bool) -> container::Style {
    let c = colors(t);
    container::Style {
        background: Some(bubble_tint(&c, mine, 0.25).into()),
        ..Default::default()
    }
}

/// Psací pole při odpovědi: obal v barvě proužku.
pub fn reply_frame(t: &Theme) -> container::Style {
    let c = colors(t);
    container::Style {
        background: Some(c.primary.into()),
        border: rounded(8.0),
        ..Default::default()
    }
}

/// Buňka tabulky ve zprávě.
pub fn table_cell(t: &Theme, mine: bool, header: bool) -> container::Style {
    let c = colors(t);
    container::Style {
        background: header.then(|| bubble_tint(&c, mine, 0.08).into()),
        border: Border {
            radius: 0.0.into(),
            width: 1.0,
            color: bubble_tint(&c, mine, 0.15),
        },
        ..Default::default()
    }
}

/// Náhled zprávy, na kterou se odpovídá, nad psacím polem (vnitřek, viz [`reply_frame`]).
pub fn reply_preview(t: &Theme) -> container::Style {
    let c = colors(t);
    container::Style {
        background: Some(c.surface_alt.into()),
        text_color: Some(c.text),
        border: Border {
            radius: Radius {
                top_left: 2.0,
                top_right: 8.0,
                bottom_right: 8.0,
                bottom_left: 2.0,
            },
            ..Default::default()
        },
        ..Default::default()
    }
}

/// Reakce pod zprávou jako tlačítko (klik přidá/odebere moji).
pub fn reaction_button(t: &Theme, status: button::Status, by_me: bool) -> button::Style {
    let c = colors(t);
    let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
    button::Style {
        background: Some(
            if by_me {
                c.primary_soft
            } else if hovered {
                c.surface_alt
            } else {
                c.surface
            }
            .into(),
        ),
        text_color: c.text,
        border: Border {
            radius: 12.0.into(),
            width: 1.0,
            color: if by_me || hovered {
                c.primary
            } else {
                c.border
            },
        },
        ..Default::default()
    }
}

/// Lišta s výběrem reakcí.
pub fn reaction_picker(t: &Theme) -> container::Style {
    let c = colors(t);
    container::Style {
        background: Some(c.surface.into()),
        border: Border {
            radius: 16.0.into(),
            width: 1.0,
            color: c.border,
        },
        shadow: Shadow {
            color: c.shadow,
            offset: Vector::new(0.0, 2.0),
            blur_radius: 8.0,
        },
        ..Default::default()
    }
}

/// Zvýraznění zprávy nalezené hledáním.
pub fn highlight(t: &Theme) -> container::Style {
    let c = colors(t);
    container::Style {
        background: Some(c.primary_soft.into()),
        border: rounded(12.0),
        ..Default::default()
    }
}

/// Tlačítko nevratné akce (smazání).
pub fn danger_button(t: &Theme, status: button::Status) -> button::Style {
    let c = colors(t);
    let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
    button::Style {
        background: Some(
            if hovered {
                c.danger
            } else {
                Color {
                    a: 0.85,
                    ..c.danger
                }
            }
            .into(),
        ),
        text_color: Color::WHITE,
        border: rounded(8.0),
        ..Default::default()
    }
}

/// Odznak s počtem nepřečtených.
pub fn badge(t: &Theme) -> container::Style {
    let c = colors(t);
    container::Style {
        background: Some(c.primary.into()),
        text_color: Some(c.on_primary),
        border: rounded(9.0),
        ..Default::default()
    }
}

/// Kolečko avataru s barvou.
pub fn avatar(bg: Color, radius: f32) -> impl Fn(&Theme) -> container::Style {
    move |_| container::Style {
        background: Some(bg.into()),
        text_color: Some(Color::WHITE),
        border: rounded(radius),
        ..Default::default()
    }
}

/// Barva avataru odvozená ze jména (stabilní mezi spuštěními).
pub fn avatar_color(name: &str) -> Color {
    const PALETTE: [Color; 10] = [
        color!(0x3B82C4),
        color!(0x2E9E8F),
        color!(0x8A5CC7),
        color!(0xC2557A),
        color!(0xD0743A),
        color!(0x4A9A4E),
        color!(0x5B6BC4),
        color!(0xB5554A),
        color!(0x2D8FB0),
        color!(0x9A7A2E),
    ];
    // FNV-1a: deterministický, na rozdíl od DefaultHasher
    let h = name.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| {
        (h ^ b as u64).wrapping_mul(0x0100_0000_01b3)
    });
    PALETTE[(h % PALETTE.len() as u64) as usize]
}

/// Iniciály pro avatar: první písmena prvních dvou slov.
pub fn initials(name: &str) -> String {
    let letters: String = name
        .split_whitespace()
        .filter_map(|w| w.chars().find(|c| c.is_alphanumeric()))
        .take(2)
        .flat_map(char::to_uppercase)
        .collect();
    if letters.is_empty() {
        "?".into()
    } else {
        letters
    }
}

// ---------- text ----------

pub fn muted(t: &Theme) -> text::Style {
    text::Style {
        color: Some(colors(t).muted),
    }
}

pub fn accent(t: &Theme) -> text::Style {
    text::Style {
        color: Some(colors(t).primary),
    }
}

pub fn danger(t: &Theme) -> text::Style {
    text::Style {
        color: Some(colors(t).danger),
    }
}

// ---------- tlačítka ----------

/// Položka seznamu (chat, kanál, osoba, záložka).
/// Druh bloku události v týdenní mřížce.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventKind {
    /// Přijatá nebo vlastní.
    Normal,
    /// Předběžná nebo bez odpovědi (obrys).
    Tentative,
    /// Zrušená nebo odmítnutá.
    Dim,
}

/// Blok události v týdenní mřížce; uplynulé události jsou tlumené.
pub fn event_block(
    t: &Theme,
    status: button::Status,
    kind: EventKind,
    selected: bool,
    past: bool,
) -> button::Style {
    let c = colors(t);
    let hover = matches!(status, button::Status::Hovered | button::Status::Pressed);
    let (bg, fg) = match kind {
        EventKind::Normal => (
            mix(c.primary_soft, c.primary, if hover { 0.35 } else { 0.18 }),
            c.text,
        ),
        EventKind::Tentative => (if hover { c.surface_alt } else { c.surface }, c.text),
        EventKind::Dim => (c.surface_alt, c.muted),
    };
    let alpha = if past { 0.6 } else { 1.0 };
    button::Style {
        background: Some(Color { a: alpha, ..bg }.into()),
        text_color: Color { a: alpha, ..fg },
        border: Border {
            color: if selected || kind == EventKind::Tentative {
                c.primary
            } else {
                Color {
                    a: 0.0,
                    ..c.primary
                }
            },
            width: if selected { 2.0 } else { 1.0 },
            radius: 5.0.into(),
        },
        ..Default::default()
    }
}

pub fn list_item(t: &Theme, status: button::Status, selected: bool) -> button::Style {
    let c = colors(t);
    let background = if selected {
        Some(c.primary_soft.into())
    } else if matches!(status, button::Status::Hovered | button::Status::Pressed) {
        Some(c.surface_alt.into())
    } else {
        None
    };
    button::Style {
        background,
        text_color: c.text,
        border: rounded(10.0),
        ..Default::default()
    }
}

/// Aktivní/neaktivní segment přepínače záložek.
pub fn segment(t: &Theme, status: button::Status, active: bool) -> button::Style {
    let c = colors(t);
    let hovered = matches!(status, button::Status::Hovered);
    button::Style {
        background: active.then_some(c.surface.into()),
        text_color: if active || hovered { c.text } else { c.muted },
        border: rounded(8.0),
        shadow: if active {
            Shadow {
                color: c.shadow,
                offset: Vector::new(0.0, 1.0),
                blur_radius: 3.0,
            }
        } else {
            Shadow::default()
        },
        ..Default::default()
    }
}

pub fn primary(t: &Theme, status: button::Status) -> button::Style {
    let c = colors(t);
    let bg = match status {
        button::Status::Hovered | button::Status::Pressed => c.primary_hover,
        button::Status::Disabled => Color {
            a: 0.4,
            ..c.primary
        },
        button::Status::Active => c.primary,
    };
    button::Style {
        background: Some(bg.into()),
        text_color: c.on_primary,
        border: rounded(10.0),
        ..Default::default()
    }
}

/// Kulaté tlačítko (odeslat, „nové zprávy“).
pub fn primary_round(t: &Theme, status: button::Status) -> button::Style {
    let mut s = primary(t, status);
    s.border = rounded(20.0);
    s.shadow = Shadow {
        color: colors(t).shadow,
        offset: Vector::new(0.0, 2.0),
        blur_radius: 6.0,
    };
    s
}

pub fn secondary(t: &Theme, status: button::Status) -> button::Style {
    let c = colors(t);
    let bg = match status {
        button::Status::Hovered | button::Status::Pressed => c.border,
        _ => c.surface_alt,
    };
    button::Style {
        background: Some(bg.into()),
        text_color: c.text,
        border: rounded(10.0),
        ..Default::default()
    }
}

/// Průhledné ikonové tlačítko (lišta, ▲▼, ✕).
pub fn ghost(t: &Theme, status: button::Status) -> button::Style {
    let c = colors(t);
    button::Style {
        background: matches!(status, button::Status::Hovered | button::Status::Pressed)
            .then_some(c.surface_alt.into()),
        text_color: if matches!(status, button::Status::Disabled) {
            Color { a: 0.35, ..c.muted }
        } else {
            c.muted
        },
        border: rounded(8.0),
        ..Default::default()
    }
}

/// Odkaz v textu (soubor, URL).
pub fn link(t: &Theme, status: button::Status, mine: bool) -> button::Style {
    let c = colors(t);
    let base = bubble_link(&c, mine);
    button::Style {
        background: None,
        text_color: if matches!(status, button::Status::Hovered) {
            Color { a: 0.8, ..base }
        } else {
            base
        },
        ..Default::default()
    }
}

// ---------- vstupy ----------

pub fn search_input(t: &Theme, status: text_input::Status) -> text_input::Style {
    let c = colors(t);
    let focused = matches!(status, text_input::Status::Focused { .. });
    text_input::Style {
        background: Background::Color(c.surface_alt),
        border: Border {
            radius: 10.0.into(),
            width: 1.0,
            color: if focused {
                c.primary
            } else {
                Color::TRANSPARENT
            },
        },
        icon: c.muted,
        placeholder: c.muted,
        value: c.text,
        selection: Color {
            a: 0.3,
            ..c.primary
        },
    }
}

/// Editor uvnitř psacího pole (rámeček kreslí okolní kontejner).
pub fn composer_editor(t: &Theme, _status: text_editor::Status) -> text_editor::Style {
    let c = colors(t);
    text_editor::Style {
        background: Background::Color(Color::TRANSPARENT),
        border: Border::default(),
        placeholder: c.muted,
        value: c.text,
        selection: Color {
            a: 0.3,
            ..c.primary
        },
    }
}

/// Text zprávy v režimu výběru: vypadá jako běžný text v bublině, jen jde označit.
pub fn selectable_text(t: &Theme, mine: bool) -> text_editor::Style {
    let c = colors(t);
    text_editor::Style {
        background: Background::Color(Color::TRANSPARENT),
        border: Border::default(),
        placeholder: c.muted,
        value: bubble_text(&c, mine),
        selection: if mine {
            Color {
                a: 0.35,
                ..Color::WHITE
            }
        } else {
            Color {
                a: 0.3,
                ..c.primary
            }
        },
    }
}

/// Kulaté tlačítko „odebrat“ v rohu miniatury: tmavé, dobře viditelné na světlém i tmavém obrázku.
pub fn remove_chip(_t: &Theme, status: button::Status) -> button::Style {
    let a = if matches!(status, button::Status::Hovered | button::Status::Pressed) {
        0.9
    } else {
        0.7
    };
    button::Style {
        background: Some(Color { a, ..Color::BLACK }.into()),
        text_color: Color::WHITE,
        border: Border {
            radius: 11.0.into(),
            width: 1.5,
            color: Color {
                a: 0.85,
                ..Color::WHITE
            },
        },
        ..Default::default()
    }
}

/// Krátké oznámení („Zkopírováno“) nad vláknem.
pub fn toast(t: &Theme) -> container::Style {
    let c = colors(t);
    container::Style {
        background: Some(c.text.into()),
        text_color: Some(c.sidebar),
        border: rounded(10.0),
        shadow: Shadow {
            color: c.shadow,
            offset: Vector::new(0.0, 4.0),
            blur_radius: 12.0,
        },
        ..Default::default()
    }
}

/// Tenký nenápadný posuvník.
pub fn thin_scrollbar(t: &Theme, status: scrollable::Status) -> scrollable::Style {
    let c = colors(t);
    let hovered = matches!(
        status,
        scrollable::Status::Hovered { .. } | scrollable::Status::Dragged { .. }
    );
    let rail = scrollable::Rail {
        background: None,
        border: Border::default(),
        scroller: scrollable::Scroller {
            background: Background::Color(if hovered {
                c.muted
            } else {
                Color { a: 0.45, ..c.muted }
            }),
            border: rounded(3.0),
        },
    };
    let mut style = scrollable::default(t, status);
    style.vertical_rail = rail;
    style.horizontal_rail = rail;
    style
}

/// Posuvník s odstupem od obsahu.
pub fn scrollbar() -> scrollable::Direction {
    scrollable::Direction::Vertical(
        scrollable::Scrollbar::new()
            .width(6)
            .scroller_width(6)
            .margin(2),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initials_and_colors() {
        assert_eq!(initials("Jan Novák"), "JN");
        assert_eq!(initials("šárka černá"), "ŠČ");
        assert_eq!(initials("Tým - porady"), "TP");
        assert_eq!(initials(""), "?");
        assert_eq!(avatar_color("Jan Novák"), avatar_color("Jan Novák"));
    }

    #[test]
    fn colors_follow_theme() {
        assert_eq!(colors(&theme(true)).app, DARK.app);
        assert_eq!(colors(&theme(false)).app, LIGHT.app);
    }
}
