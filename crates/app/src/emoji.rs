//! Výběr emoji v psacím poli: kategorie a naposledy použité; rozpoznání emoji v textu.

use std::ops::Range;

use iced::advanced::text::highlighter::{self, Format};
use iced::{Font, Theme};

/// Kategorie: název (popisek), ikona záložky, emoji.
pub const CATEGORIES: &[(&str, &str, &[&str])] = &[
    (
        "Smajlíci",
        "😀",
        &[
            "😀",
            "😃",
            "😄",
            "😁",
            "😆",
            "😅",
            "😂",
            "🤣",
            "😊",
            "😇",
            "🙂",
            "🙃",
            "😉",
            "😌",
            "😍",
            "🥰",
            "😘",
            "😋",
            "😛",
            "😜",
            "🤪",
            "😝",
            "🤗",
            "🤭",
            "🤫",
            "🤔",
            "🤐",
            "🤨",
            "😐",
            "😑",
            "😶",
            "😏",
            "😒",
            "🙄",
            "😬",
            "😮‍💨",
            "🤥",
            "😴",
            "😷",
            "🤒",
            "🤕",
            "🤢",
            "🤧",
            "🥵",
            "🥶",
            "😵",
            "🤯",
            "🤠",
            "🥳",
            "😎",
            "🤓",
            "🧐",
            "😕",
            "😟",
            "🙁",
            "😮",
            "😯",
            "😲",
            "😳",
            "🥺",
            "😦",
            "😧",
            "😨",
            "😰",
            "😥",
            "😢",
            "😭",
            "😱",
            "😖",
            "😣",
            "😞",
            "😓",
            "😩",
            "😫",
            "🥱",
            "😤",
            "😡",
            "😠",
            "🤬",
            "😈",
            "💀",
            "💩",
            "🤡",
            "👻",
        ],
    ),
    (
        "Gesta a lidé",
        "👍",
        &[
            "👍",
            "👎",
            "👌",
            "🤌",
            "✌️",
            "🤞",
            "🤟",
            "🤘",
            "🤙",
            "👈",
            "👉",
            "👆",
            "👇",
            "☝️",
            "✋",
            "🤚",
            "🖐️",
            "🖖",
            "👋",
            "👏",
            "🙌",
            "👐",
            "🤲",
            "🤝",
            "🙏",
            "✍️",
            "💪",
            "🫶",
            "👀",
            "🧠",
            "🫡",
            "🤷",
            "🤦",
            "🙋",
            "🙆",
            "🙅",
            "💁",
            "🙇",
            "🧑‍💻",
            "👨‍💻",
            "👩‍💻",
            "🧑‍🏫",
            "🏃",
            "🚶",
            "🧘",
            "🕺",
            "💃",
            "👪",
            "🎅",
            "🦸",
        ],
    ),
    (
        "Srdce a symboly",
        "❤️",
        &[
            "❤️", "🧡", "💛", "💚", "💙", "💜", "🖤", "🤍", "🤎", "💔", "❣️", "💕", "💖", "💯",
            "✅", "☑️", "✔️", "❌", "❎", "⚠️", "❗", "❓", "‼️", "⁉️", "💡", "🔥", "⭐", "🌟",
            "✨", "⚡", "💥", "🎯", "🚀", "🎉", "🎊", "🏆", "🥇", "📌", "📎", "🔗", "🔒", "🔑",
            "⏰", "⏳", "📅", "📈", "📉", "🆗", "🆕", "🆘", "➡️", "⬅️", "⬆️", "⬇️", "🔄", "➕",
        ],
    ),
    (
        "Příroda a jídlo",
        "🌿",
        &[
            "☀️", "🌤️", "⛅", "🌧️", "⛈️", "❄️", "☃️", "🌈", "🌊", "🌍", "🌙", "🌸", "🌻", "🌹",
            "🌿", "🍀", "🌲", "🍁", "🐶", "🐱", "🐭", "🐰", "🦊", "🐻", "🐼", "🐨", "🐯", "🦁",
            "🐮", "🐷", "🐸", "🐵", "🙈", "🙉", "🙊", "🐔", "🐧", "🐦", "🦄", "🐝", "🐛", "🦋",
            "🍎", "🍌", "🍓", "🍒", "🍑", "🍍", "🥑", "🥕", "🌶️", "🍞", "🧀", "🍕", "🍔", "🍟",
            "🌭", "🌮", "🍝", "🍣", "🍰", "🎂", "🍩", "🍪", "🍫", "🍿", "☕", "🍵", "🍺", "🍻",
            "🥂", "🍷", "🥃", "🍸",
        ],
    ),
    (
        "Věci a činnosti",
        "💼",
        &[
            "💼", "💻", "🖥️", "⌨️", "🖱️", "📱", "☎️", "📞", "📧", "📨", "📝", "📄", "📊", "📋",
            "📁", "🗂️", "🗓️", "📚", "✏️", "🖊️", "🔍", "🛠️", "🔧", "🔨", "⚙️", "🧪", "🐛", "🧩",
            "🎮", "🎧", "🎵", "🎬", "📷", "🎁", "🎈", "🏠", "🏢", "🚗", "🚲", "✈️", "🚆", "⛱️",
            "⚽", "🏀", "🎾", "🏓", "⛷️", "🏊", "🚴", "🧗", "💰", "💳", "🛒", "🍾",
        ],
    ),
];

/// Kolik naposledy použitých emoji pamatovat.
pub const RECENT_MAX: usize = 20;
/// Emoji v řádku mřížky.
pub const PER_ROW: usize = 10;

/// Úseky textu, které jsou emoji (bajtové rozsahy). Vykreslují se barevným fontem
/// [`crate::style::EMOJI`]; jinak by je iced vzal z prvního fontu, který glyf má, a ten
/// bývá černobílý.
pub fn ranges(s: &str) -> Vec<Range<usize>> {
    let mut out: Vec<Range<usize>> = vec![];
    let mut prev = false;
    for (i, c) in s.char_indices() {
        let mut rest = s[i + c.len_utf8()..].chars();
        let (next, next2) = (rest.next(), rest.next());
        let emoji = if attaches(c) {
            prev
        } else if matches!(c, '0'..='9' | '#' | '*') {
            // klávesnicová emoji 1️⃣
            next == Some('\u{20E3}') || (next == Some('\u{FE0F}') && next2 == Some('\u{20E3}'))
        } else if next == Some('\u{FE0E}') {
            false
        } else {
            strong(c) || (next == Some('\u{FE0F}') && !c.is_alphanumeric() && !c.is_whitespace())
        };
        if emoji {
            let end = i + c.len_utf8();
            match out.last_mut() {
                Some(r) if r.end == i => r.end = end,
                _ => out.push(i..end),
            }
        }
        prev = emoji;
    }
    out
}

/// Zvýraznění pro `text_editor`: emoji barevným fontem (řádky jsou na sobě nezávislé).
pub struct Highlighter {
    line: usize,
}

impl highlighter::Highlighter for Highlighter {
    type Settings = ();
    type Highlight = ();
    type Iterator<'a> = std::vec::IntoIter<(Range<usize>, ())>;

    fn new(_: &()) -> Self {
        Self { line: 0 }
    }

    fn update(&mut self, _: &()) {
        self.line = 0;
    }

    fn change_line(&mut self, line: usize) {
        self.line = self.line.min(line);
    }

    fn highlight_line(&mut self, line: &str) -> Self::Iterator<'_> {
        self.line += 1;
        let spans: Vec<_> = ranges(line).into_iter().map(|r| (r, ())).collect();
        spans.into_iter()
    }

    fn current_line(&self) -> usize {
        self.line
    }
}

/// Formát zvýrazněného úseku: barevný emoji font.
pub fn format(_: &(), _: &Theme) -> Format<Font> {
    Format {
        color: None,
        font: Some(crate::style::EMOJI),
    }
}

/// Text rozdělený na úseky `(text, je emoji)`.
pub fn runs(s: &str) -> Vec<(&str, bool)> {
    let mut out = vec![];
    let mut at = 0;
    for r in ranges(s) {
        if r.start > at {
            out.push((&s[at..r.start], false));
        }
        out.push((&s[r.clone()], true));
        at = r.end;
    }
    if at < s.len() || out.is_empty() {
        out.push((&s[at..], false));
    }
    out
}

/// Znak, který se jako emoji zobrazuje i bez U+FE0F.
fn strong(c: char) -> bool {
    matches!(c as u32,
        0x1F000..=0x1F0FF | 0x1F170..=0x1FAFF
        | 0x2600..=0x27BF
        | 0x231A | 0x231B | 0x2328 | 0x23CF | 0x23E9..=0x23F3 | 0x23F8..=0x23FA
        | 0x2B05..=0x2B07 | 0x2B1B | 0x2B1C | 0x2B50 | 0x2B55
        | 0x3030 | 0x303D | 0x3297 | 0x3299)
}

/// Pokračování emoji sekvence (spojovač, varianta, barva pleti, keycap, vlajkové tagy).
fn attaches(c: char) -> bool {
    matches!(c as u32,
        0x200D | 0xFE0F | 0xFE0E | 0x20E3 | 0x1F3FB..=0x1F3FF | 0xE0020..=0xE007F)
}

/// Zařadí emoji na začátek naposledy použitých.
pub fn remember(recent: &mut Vec<String>, emoji: &str) {
    recent.retain(|e| e != emoji);
    recent.insert(0, emoji.to_owned());
    recent.truncate(RECENT_MAX);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recent_moves_to_front_and_is_bounded() {
        let mut r = vec![];
        for i in 0..30 {
            remember(&mut r, &i.to_string());
        }
        assert_eq!(r.len(), RECENT_MAX);
        assert_eq!(r[0], "29");
        remember(&mut r, "25");
        assert_eq!(r[0], "25");
        assert_eq!(r.iter().filter(|e| *e == "25").count(), 1);
    }

    #[test]
    fn finds_emoji_runs() {
        assert_eq!(runs("Bože 🤣"), vec![("Bože ", false), ("🤣", true)]);
        assert_eq!(runs("ahoj"), vec![("ahoj", false)]);
        assert_eq!(runs(""), vec![("", false)]);
        // spojené sekvence, barva pleti, vlajka a FE0F zůstanou v jednom úseku
        assert_eq!(
            runs("a👨‍💻👍🏽🇨🇿❤️b"),
            vec![("a", false), ("👨‍💻👍🏽🇨🇿❤️", true), ("b", false)]
        );
        assert_eq!(runs("1️⃣ 1"), vec![("1️⃣", true), (" 1", false)]);
        // šipky a © bez FE0F jsou text, s FE0F emoji; FE0E vynutí text
        assert_eq!(runs("a → b ©"), vec![("a → b ©", false)]);
        assert_eq!(runs("➡️"), vec![("➡️", true)]);
        assert_eq!(runs("☀︎"), vec![("☀︎", false)]);
        assert!(ranges("text, čárky a – pomlčky").is_empty());
    }

    #[test]
    fn picker_emoji_are_recognized_whole() {
        for (name, _, list) in CATEGORIES {
            for e in *list {
                assert_eq!(runs(e), vec![(*e, true)], "{name}: {e}");
            }
        }
    }

    #[test]
    fn categories_have_no_duplicates_within() {
        for (name, _, list) in CATEGORIES {
            let mut seen = std::collections::HashSet::new();
            for e in *list {
                assert!(seen.insert(e), "{name}: {e} dvakrát");
            }
        }
    }
}
