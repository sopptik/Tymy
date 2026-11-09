//! Formátování při psaní: jednoduchý Markdown z psacího pole → HTML Teams, a zpět
//! (z obsahu zprávy do psacího pole při úpravě).
//!
//! Podporováno: `**tučně**`, `__tučně__`, `*kurzíva*`, `_kurzíva_`, `~~přeškrtnuté~~`,
//! `` `kód` ``, bloky kódu mezi ```` ``` ````, odrážky (`- `, `* `, `• `), číslované seznamy
//! (aspoň dva řádky `1. `), citace (`> `), odkazy `[text](https://…)` a holé adresy, zmínky
//! `@Jméno`. Vše ostatní zůstane prostým textem (zalomení řádků jako `<br>`).

use super::rich::{Block, QuoteKind, Span};
use crate::MentionRef;

/// HTML zprávy z textu psacího pole; vrací i použité zmínky v pořadí `itemid`.
pub fn to_html(text: &str, mentions: &[MentionRef]) -> (String, Vec<MentionRef>) {
    let mut inline = Inline::new(mentions);
    let lines: Vec<&str> = text.trim_end().lines().collect();
    let mut out = String::new();
    let mut para: Vec<String> = vec![];
    let flush = |out: &mut String, para: &mut Vec<String>| {
        if !para.is_empty() {
            out.push_str("<p>");
            out.push_str(&para.join("<br>"));
            out.push_str("</p>");
            para.clear();
        }
    };
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        let t = line.trim_start();
        if let Some(lang) = t.strip_prefix("```") {
            let mut code = vec![];
            i += 1;
            while i < lines.len() && !lines[i].trim_start().starts_with("```") {
                code.push(lines[i]);
                i += 1;
            }
            i += 1; // uzavírací ``` (nebo konec textu)
            flush(&mut out, &mut para);
            let lang = lang.trim();
            let class = if lang.is_empty() {
                String::new()
            } else {
                format!(r#" class="language-{}""#, escape_attr(lang))
            };
            out.push_str(&format!(
                "<pre{class}><code>{}</code></pre>",
                escape(&code.join("\n"))
            ));
            continue;
        }
        if bullet(t).is_some() {
            flush(&mut out, &mut para);
            out.push_str("<ul>");
            while let Some(item) = lines.get(i).and_then(|l| bullet(l.trim_start())) {
                out.push_str(&format!("<li>{}</li>", inline.render(item)));
                i += 1;
            }
            out.push_str("</ul>");
            continue;
        }
        // číslovaný seznam až od dvou řádků („9. 11. máme poradu“ seznam není)
        if numbered(t).is_some()
            && lines
                .get(i + 1)
                .is_some_and(|l| numbered(l.trim_start()).is_some())
        {
            flush(&mut out, &mut para);
            out.push_str("<ol>");
            while let Some(item) = lines.get(i).and_then(|l| numbered(l.trim_start())) {
                out.push_str(&format!("<li>{}</li>", inline.render(item)));
                i += 1;
            }
            out.push_str("</ol>");
            continue;
        }
        if quoted(t).is_some() {
            flush(&mut out, &mut para);
            let mut inner = vec![];
            while let Some(q) = lines.get(i).and_then(|l| quoted(l.trim_start())) {
                inner.push(inline.render(q));
                i += 1;
            }
            out.push_str(&format!(
                "<blockquote><p>{}</p></blockquote>",
                inner.join("<br>")
            ));
            continue;
        }
        para.push(inline.render(line));
        i += 1;
    }
    flush(&mut out, &mut para);
    if out.is_empty() {
        out.push_str("<p></p>");
    }
    (out, inline.used)
}

fn bullet(t: &str) -> Option<&str> {
    ["- ", "* ", "• "].iter().find_map(|m| t.strip_prefix(m))
}

fn numbered(t: &str) -> Option<&str> {
    let digits = t.chars().take_while(char::is_ascii_digit).count();
    if digits == 0 || digits > 3 {
        return None;
    }
    t[digits..]
        .strip_prefix(". ")
        .or_else(|| t[digits..].strip_prefix(") "))
}

fn quoted(t: &str) -> Option<&str> {
    t.strip_prefix("> ").or_else(|| (t == ">").then_some(""))
}

/// Řádkové formátování a zmínky.
struct Inline<'m> {
    /// Zmínky od nejdelšího jména („@Jan Novák“ před „@Jan“).
    mentions: Vec<&'m MentionRef>,
    used: Vec<MentionRef>,
}

impl<'m> Inline<'m> {
    fn new(mentions: &'m [MentionRef]) -> Self {
        let mut mentions: Vec<&MentionRef> =
            mentions.iter().filter(|m| !m.name.is_empty()).collect();
        mentions.sort_by_key(|m| std::cmp::Reverse(m.name.len()));
        Self {
            mentions,
            used: vec![],
        }
    }

    fn render(&mut self, s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        let mut prev: Option<char> = None;
        let mut i = 0;
        while let Some(c) = s[i..].chars().next() {
            let rest = &s[i..];
            let at_word_start = prev.is_none_or(|p| !p.is_alphanumeric());
            // kód v řádku: obsah doslova
            if c == '`'
                && let Some(end) = rest[1..].find('`').filter(|e| *e > 0)
            {
                out.push_str(&format!("<code>{}</code>", escape(&rest[1..1 + end])));
                i += end + 2;
                prev = Some('`');
                continue;
            }
            if at_word_start && let Some((len, tag, inner)) = emphasis(rest) {
                out.push_str(&format!("<{tag}>{}</{tag}>", self.render(inner)));
                i += len;
                prev = rest[..len].chars().last();
                continue;
            }
            if c == '['
                && let Some((len, label, url)) = md_link(rest)
            {
                out.push_str(&link(url, &self.render(label)));
                i += len;
                prev = Some(')');
                continue;
            }
            if at_word_start && (rest.starts_with("https://") || rest.starts_with("http://")) {
                let url = bare_url(rest);
                out.push_str(&link(url, &escape(url)));
                i += url.len();
                prev = url.chars().last();
                continue;
            }
            if c == '@' && prev.is_none_or(|p| p.is_whitespace() || "([{\"'".contains(p)) {
                let after = &rest[1..];
                let found = self.mentions.iter().find(|m| {
                    after.starts_with(m.name.as_str())
                        && after[m.name.len()..]
                            .chars()
                            .next()
                            .is_none_or(|n| !n.is_alphanumeric())
                });
                if let Some(m) = found {
                    out.push_str(&format!(
                        r#"<span itemtype="http://schema.skype.com/Mention" itemscope="" itemid="{}">{}</span>"#,
                        self.used.len(),
                        escape(&m.name)
                    ));
                    self.used.push((*m).clone());
                    i += 1 + m.name.len();
                    prev = m.name.chars().last();
                    continue;
                }
            }
            out.push_str(&escape(c.encode_utf8(&mut [0; 4])));
            prev = Some(c);
            i += c.len_utf8();
        }
        out
    }
}

/// `**x**`, `__x__`, `~~x~~`, `*x*`, `_x_` na začátku `s`: (délka, značka, obsah).
/// Obsah nesmí začínat ani končit mezerou a za uzavírací značkou nesmí být písmeno
/// (`snake_case_jmeno` zůstane textem).
fn emphasis(s: &str) -> Option<(usize, &'static str, &str)> {
    let (delim, tag) = [
        ("**", "b"),
        ("__", "b"),
        ("~~", "s"),
        ("*", "i"),
        ("_", "i"),
    ]
    .into_iter()
    .find(|(d, _)| s.starts_with(d))?;
    let body = &s[delim.len()..];
    if body.starts_with(char::is_whitespace) || body.starts_with(delim) {
        return None;
    }
    let single = delim.len() == 1;
    let mut from = 0;
    while let Some(pos) = body[from..].find(delim) {
        let j = from + pos;
        let after = &body[j + delim.len()..];
        let inner = &body[..j];
        let doubled = single && after.starts_with(delim);
        let ok = !inner.is_empty()
            && !inner.ends_with(char::is_whitespace)
            && !doubled
            && after.chars().next().is_none_or(|n| !n.is_alphanumeric());
        if ok {
            return Some((delim.len() * 2 + j, tag, inner));
        }
        from = j + delim.len() + usize::from(doubled);
        if from >= body.len() {
            break;
        }
    }
    None
}

/// `[text](https://…)` na začátku `s`: (délka, text, adresa).
fn md_link(s: &str) -> Option<(usize, &str, &str)> {
    let close = s.find("](")?;
    let label = &s[1..close];
    let rest = &s[close + 2..];
    let end = rest.find(')')?;
    let url = rest[..end].trim();
    let allowed = ["https://", "http://", "mailto:"]
        .iter()
        .any(|p| url.starts_with(p));
    (allowed && !label.is_empty() && !url.contains(char::is_whitespace)).then_some((
        close + 2 + end + 1,
        label,
        url,
    ))
}

/// Holá adresa až do mezery, bez interpunkce na konci (`viz https://x.cz.` → bez tečky).
fn bare_url(s: &str) -> &str {
    let end = s.find(char::is_whitespace).unwrap_or(s.len());
    let mut url = &s[..end];
    loop {
        let trimmed = url.trim_end_matches(['.', ',', ';', ':', '!', '?', '"', '\'']);
        let trimmed = if trimmed.ends_with(')') && !trimmed.contains('(') {
            &trimmed[..trimmed.len() - 1]
        } else {
            trimmed
        };
        if trimmed.len() == url.len() {
            return url;
        }
        url = trimmed;
    }
}

fn link(url: &str, label_html: &str) -> String {
    let href = escape_attr(url);
    format!(
        r#"<a href="{href}" rel="noreferrer noopener" target="_blank" title="{href}">{label_html}</a>"#
    )
}

fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            '\r' => {}
            c => out.push(c),
        }
    }
    out
}

fn escape_attr(s: &str) -> String {
    escape(s)
}

/// Obsah zprávy jako Markdown pro psací pole (úprava zprávy). Citace odpovědi a obrázky
/// vynechá (při uložení se převezmou z původní zprávy), zmínky zůstanou jako „@Jméno“.
pub fn from_blocks(blocks: &[Block]) -> String {
    let mut lines: Vec<String> = vec![];
    for block in blocks {
        match block {
            Block::Paragraph(s) | Block::Heading(s) => lines.push(spans_md(s)),
            Block::ListItem {
                depth,
                marker,
                spans,
            } => {
                let indent = "  ".repeat(depth.saturating_sub(1));
                let marker = if marker.is_empty() {
                    "  ".to_owned()
                } else if marker.starts_with(|c: char| c.is_ascii_digit()) {
                    format!("{marker} ")
                } else {
                    "- ".to_owned()
                };
                lines.push(format!("{indent}{marker}{}", spans_md(spans)));
            }
            Block::Code { language, text } => {
                lines.push(format!("```{}", language.as_deref().unwrap_or_default()));
                lines.push(text.trim_end_matches('\n').to_owned());
                lines.push("```".into());
            }
            Block::Quote(q) => match q.kind {
                QuoteKind::Reply { .. } => {}
                _ => lines.extend(from_blocks(&q.blocks).lines().map(|l| format!("> {l}"))),
            },
            Block::Table(rows) => {
                for row in rows {
                    let cells: Vec<String> = row.iter().map(|c| spans_md(&c.spans)).collect();
                    lines.push(cells.join(" | "));
                }
            }
            Block::Image(_) => {}
            Block::Rule => lines.push("---".into()),
            Block::Blank => lines.push(String::new()),
        }
    }
    // prázdné řádky na začátku a konci (např. po vynechané citaci) pryč
    let text = lines.join("\n");
    text.trim_matches('\n').to_owned()
}

fn spans_md(spans: &[Span]) -> String {
    let mut out = String::new();
    for s in spans {
        // mezery na krajích patří mimo značky („**a** b“, ne „**a **b“)
        let core = s.text.trim();
        if core.is_empty() {
            out.push_str(&s.text);
            continue;
        }
        let lead = &s.text[..s.text.len() - s.text.trim_start().len()];
        let trail = &s.text[s.text.trim_end().len()..];
        let mut t = if s.style.code {
            format!("`{core}`")
        } else if s.style.mention && !core.starts_with('@') {
            format!("@{core}")
        } else {
            core.to_owned()
        };
        if !s.style.code {
            if s.style.strike {
                t = format!("~~{t}~~");
            }
            if s.style.italic {
                t = format!("_{t}_");
            }
            if s.style.bold {
                t = format!("**{t}**");
            }
        }
        if let Some(url) = &s.link
            && !s.style.mention
        {
            t = if core == url {
                url.clone()
            } else {
                format!("[{t}]({url})")
            };
        }
        out.push_str(lead);
        out.push_str(&t);
        out.push_str(trail);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::rich;

    fn html(s: &str) -> String {
        to_html(s, &[]).0
    }

    #[test]
    fn plain_text_stays_plain() {
        assert_eq!(
            html("a < b & \"c\"\ndruhý řádek\n"),
            "<p>a &lt; b &amp; &quot;c&quot;<br>druhý řádek</p>"
        );
        assert_eq!(html("a\n\nb"), "<p>a<br><br>b</p>");
        assert_eq!(html(""), "<p></p>");
        assert_eq!(
            html("snake_case_jmeno a 2*3*4"),
            "<p>snake_case_jmeno a 2*3*4</p>"
        );
        assert_eq!(html("9. 11. máme poradu"), "<p>9. 11. máme poradu</p>");
        assert_eq!(html("cena * 2"), "<p>cena * 2</p>");
    }

    #[test]
    fn inline_formatting() {
        assert_eq!(
            html("**tučně** _kurzíva_ *taky* ~~pryč~~ `a<b>` __b__"),
            "<p><b>tučně</b> <i>kurzíva</i> <i>taky</i> <s>pryč</s> <code>a&lt;b&gt;</code> <b>b</b></p>"
        );
        assert_eq!(
            html("**tučně _a kurzíva_**"),
            "<p><b>tučně <i>a kurzíva</i></b></p>"
        );
        assert_eq!(html("** ne**"), "<p>** ne**</p>");
        assert_eq!(html("`**kód**`"), "<p><code>**kód**</code></p>");
    }

    #[test]
    fn links() {
        let a = |u: &str, t: &str| {
            format!(
                r#"<a href="{u}" rel="noreferrer noopener" target="_blank" title="{u}">{t}</a>"#
            )
        };
        assert_eq!(
            html("viz https://example.com/a_b?x=1&y=2."),
            format!(
                "<p>viz {}.</p>",
                a(
                    "https://example.com/a_b?x=1&amp;y=2",
                    "https://example.com/a_b?x=1&amp;y=2"
                )
            )
        );
        assert_eq!(
            html("[**web**](https://x.cz)"),
            format!("<p>{}</p>", a("https://x.cz", "<b>web</b>"))
        );
        assert_eq!(
            html("[zlý](javascript:alert(1))"),
            "<p>[zlý](javascript:alert(1))</p>"
        );
        assert_eq!(
            html("(https://x.cz/a)"),
            format!("<p>({})</p>", a("https://x.cz/a", "https://x.cz/a"))
        );
    }

    #[test]
    fn blocks() {
        assert_eq!(
            html("Seznam:\n- jedna\n* **dvě**\nkonec"),
            "<p>Seznam:</p><ul><li>jedna</li><li><b>dvě</b></li></ul><p>konec</p>"
        );
        assert_eq!(html("1. a\n2) b"), "<ol><li>a</li><li>b</li></ol>");
        assert_eq!(
            html("> citace\n> _dál_\nodpověď"),
            "<blockquote><p>citace<br><i>dál</i></p></blockquote><p>odpověď</p>"
        );
        assert_eq!(
            html("kód:\n```rust\nlet a = 1 < 2;\n  **ne**\n```\npo"),
            "<p>kód:</p><pre class=\"language-rust\"><code>let a = 1 &lt; 2;\n  **ne**</code></pre><p>po</p>"
        );
    }

    #[test]
    fn mentions_inside_formatting() {
        let jan = MentionRef {
            mri: "8:orgid:j".into(),
            name: "Jan Novák".into(),
        };
        let (h, used) = to_html(
            "**@Jan Novák** a mail@Jan Novák",
            std::slice::from_ref(&jan),
        );
        assert_eq!(
            h,
            "<p><b><span itemtype=\"http://schema.skype.com/Mention\" itemscope=\"\" itemid=\"0\">Jan Novák</span></b> a mail@Jan Novák</p>"
        );
        assert_eq!(used, vec![jan]);
    }

    #[test]
    fn roundtrip_through_editing() {
        for md in [
            "**tučně** a _kurzíva_ a ~~pryč~~ a `kód`",
            "- jedna\n- **dvě**",
            "1. a\n2. b",
            "> citace",
            "```\nlet a = 1;\n```",
            "[web](https://x.cz) a https://y.cz",
            "řádek\ndruhý",
        ] {
            let back = from_blocks(&rich::parse(&html(md)));
            assert_eq!(back, md, "z HTML: {}", html(md));
        }
    }

    #[test]
    fn editing_skips_reply_quote_and_keeps_mentions_as_text() {
        let original = r#"<blockquote itemscope="" itemtype="http://schema.skype.com/Reply" itemid="1"><strong itemprop="mri" itemid="8:orgid:x">Petr</strong><p itemprop="preview">původní</p></blockquote><p>Ahoj <span itemtype="http://schema.skype.com/Mention" itemscope="" itemid="0">Jan</span>, <b>pozor</b></p>"#;
        assert_eq!(from_blocks(&rich::parse(original)), "Ahoj @Jan, **pozor**");
    }
}
