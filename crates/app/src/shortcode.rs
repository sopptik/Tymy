//! Emoji z textu v psacím poli: zkratky jako na GitHubu (`:smile:`, nabídka po „:sm“)
//! a převod smajlíků ze znaků (`:-)` → 🙂).

/// Nejkratší dotaz za „:“, při kterém nabízíme emoji (GitHub také od dvou znaků;
/// kratší by se pletly s časem „10:3“ a dvojtečkou ve větě).
const MIN_QUERY_CHARS: usize = 2;
const MAX_QUERY_CHARS: usize = 30;

/// Smajlíky ze znaků → emoji. Převádí se jen samostatné slovo (před ním začátek řádku
/// nebo mezera, za ním mezera / nový řádek / odeslání).
const EMOTICONS: &[(&str, &str)] = &[
    (":-)", "🙂"),
    (":)", "🙂"),
    (":-D", "😃"),
    (":D", "😃"),
    (";-)", "😉"),
    (";)", "😉"),
    (":-(", "🙁"),
    (":(", "🙁"),
    (":'(", "😢"),
    (":-P", "😛"),
    (":P", "😛"),
    (":-p", "😛"),
    (":p", "😛"),
    (":-O", "😮"),
    (":O", "😮"),
    (":-o", "😮"),
    (":o", "😮"),
    (":-*", "😘"),
    (":*", "😘"),
    (":-|", "😐"),
    (":|", "😐"),
    (":-/", "😕"),
    (":/", "😕"),
    ("B-)", "😎"),
    ("8-)", "😎"),
    ("xD", "😆"),
    ("XD", "😆"),
    (":-$", "😳"),
    (">:(", "😠"),
    ("<3", "❤️"),
    ("</3", "💔"),
    ("(y)", "👍"),
    ("(Y)", "👍"),
    ("(n)", "👎"),
    ("(N)", "👎"),
];

/// Rozepsaná zkratka: bajtová pozice „:“ v řádku a text za ní až po kurzor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Query {
    pub line: usize,
    pub at: usize,
    pub text: String,
}

fn code_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '+' | '-')
}

/// Smí před „:“ / smajlíkem být tenhle znak (začátek slova)?
fn word_start(prev: Option<char>) -> bool {
    prev.is_none_or(|p| p.is_whitespace() || "([{\"'".contains(p))
}

/// Je pozice uvnitř `kódu` (lichý počet zpětných apostrofů před ní)?
fn in_code(before: &str) -> bool {
    before.matches('`').count() % 2 == 1
}

/// Najde rozepsanou zkratku `:xy…` před kurzorem (`col` = bajtový index).
pub fn query_at(line: usize, line_text: &str, col: usize) -> Option<Query> {
    let before = line_text.get(..col.min(line_text.len()))?;
    let at = before.rfind(':')?;
    let text = &before[at + 1..];
    let n = text.chars().count();
    if !(MIN_QUERY_CHARS..=MAX_QUERY_CHARS).contains(&n)
        || !text.chars().all(code_char)
        // „:-P“, „:-D“ jsou smajlíky, ne zkratky
        || text.starts_with('-')
        || !word_start(before[..at].chars().last())
        || in_code(&before[..at])
    {
        return None;
    }
    Some(Query {
        line,
        at,
        text: text.to_owned(),
    })
}

/// Emoji k dotazu: (emoji, zkratka). Nejdřív zkratky začínající dotazem, pak ty, kde
/// dotazem začíná některá část (`_`), nakonec obsahující; kratší dřív.
pub fn suggestions(query: &str, max: usize) -> Vec<(&'static str, &'static str)> {
    let q = query.to_ascii_lowercase();
    let mut found: Vec<(u8, usize, &'static str, &'static str)> = vec![];
    for e in emojis::iter() {
        let best = e
            .shortcodes()
            .filter_map(|code| {
                let rank = if code.starts_with(&q) {
                    0
                } else if code.split(['_', '-']).any(|part| part.starts_with(&q)) {
                    1
                } else if code.contains(&q) {
                    2
                } else {
                    return None;
                };
                Some((rank, code.len(), code))
            })
            .min();
        if let Some((rank, len, code)) = best {
            found.push((rank, len, e.as_str(), code));
        }
    }
    found.sort();
    found
        .into_iter()
        .take(max)
        .map(|(_, _, e, c)| (e, c))
        .collect()
}

/// Právě dopsaná zkratka `:kód:` končící u kurzoru → (počet znaků k nahrazení, emoji).
pub fn completed_at(line_text: &str, col: usize) -> Option<(usize, &'static str)> {
    let before = line_text.get(..col.min(line_text.len()))?;
    let inner = before.strip_suffix(':')?;
    let at = inner.rfind(':')?;
    let code = &inner[at + 1..];
    if code.is_empty()
        || !code.chars().all(code_char)
        || !word_start(inner[..at].chars().last())
        || in_code(&inner[..at])
    {
        return None;
    }
    let e = emojis::get_by_shortcode(code)?;
    Some((code.chars().count() + 2, e.as_str()))
}

/// Smajlík těsně před posledním (právě napsaným) znakem `col` → (jeho délka ve znacích,
/// emoji). Volá se po napsání mezery nebo nového řádku.
pub fn emoticon_before(line_text: &str, col: usize) -> Option<(usize, &'static str)> {
    let before = line_text.get(..col.min(line_text.len()))?;
    let mut chars = before.chars();
    chars.next_back().filter(|c| c.is_whitespace())?;
    emoticon_at_end(chars.as_str())
}

/// Smajlík na konci textu (samostatné slovo).
fn emoticon_at_end(before: &str) -> Option<(usize, &'static str)> {
    EMOTICONS.iter().find_map(|(from, to)| {
        let rest = before.strip_suffix(from)?;
        (word_start(rest.chars().last()) && !in_code(rest)).then(|| (from.chars().count(), *to))
    })
}

/// Při odeslání: smajlík na úplném konci zprávy (za ním už mezera nepřišla).
pub fn convert_trailing(text: &str) -> String {
    match emoticon_at_end(text) {
        Some((n, emoji)) => {
            let cut = text.char_indices().rev().nth(n - 1).map_or(0, |(i, _)| i);
            format!("{}{emoji}", &text[..cut])
        }
        None => text.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_shortcode_query() {
        let q = |s: &str| query_at(0, s, s.len()).map(|q| q.text);
        assert_eq!(q("ahoj :sm"), Some("sm".into()));
        assert_eq!(q(":+1"), Some("+1".into()));
        assert_eq!(q("ahoj :s"), None, "jeden znak je málo");
        assert_eq!(q("v 10:30"), None, "čas");
        assert_eq!(q("pozor:ab"), None, "dvojtečka uprostřed slova");
        assert_eq!(q("ahoj :sm il"), None);
        assert_eq!(q("`kód :sm"), None, "uvnitř kódu");
        assert_eq!(q("ahoj :-P"), None, "smajlík");
    }

    #[test]
    fn suggests_github_shortcodes() {
        let s = suggestions("smil", 8);
        assert_eq!(s.first().map(|x| x.1), Some("smile"));
        assert!(s.iter().any(|(e, _)| *e == "😄"));
        assert!(suggestions("thumbsup", 3).iter().any(|(e, _)| *e == "👍"));
        // část názvu: „heart_eyes“ najde i „eyes“
        assert!(
            suggestions("eyes", 20)
                .iter()
                .any(|(_, c)| *c == "heart_eyes")
        );
        assert!(suggestions("zzzzqq", 8).is_empty());
    }

    #[test]
    fn completes_typed_shortcode() {
        let s = "ahoj :smile:";
        assert_eq!(completed_at(s, s.len()), Some((7, "😄")));
        assert_eq!(completed_at("a :neexistuje:", 14), None);
        assert_eq!(completed_at("10:30:", 6), None);
    }

    #[test]
    fn converts_emoticons() {
        let s = "ahoj :-) ";
        assert_eq!(emoticon_before(s, s.len()), Some((3, "🙂")));
        assert_eq!(emoticon_before(":D\n", 3), Some((2, "😃")));
        assert_eq!(emoticon_before("<3 ", 3), Some((2, "❤️")));
        assert_eq!(emoticon_before("http:/ ", 7), None, "uprostřed slova");
        assert_eq!(emoticon_before("ahoj :-)", 8), None, "bez mezery za");
        assert_eq!(emoticon_before("`:-) ", 6), None, "v kódu");
        assert_eq!(convert_trailing("díky :)"), "díky 🙂");
        assert_eq!(convert_trailing("díky (y)"), "díky 👍");
        assert_eq!(convert_trailing("viz https://x.cz/"), "viz https://x.cz/");
        assert_eq!(convert_trailing(":-("), "🙁");
    }
}
