//! Převody mezi prostým textem a HTML obsahem zpráv.
//!
//! Plnohodnotné vykreslení rich textu bude v UI; tady je jen to, co potřebuje
//! odesílání, CLI a náhledy v seznamu chatů. Strukturovaný obsah pro okno je v [`rich`].

pub mod rich;

/// Escapuje prostý text do HTML zprávy Teams (`<p>…</p>`, nové řádky jako `<br>`).
pub fn plain_to_html(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 7);
    out.push_str("<p>");
    for c in text.trim_end().chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            '\n' => out.push_str("<br>"),
            '\r' => {}
            c => out.push(c),
        }
    }
    out.push_str("</p>");
    out
}

/// HTML citace pro odpověď na zprávu (stejný tvar, jaký posílá web klient Teams).
/// Patří před vlastní text odpovědi.
pub fn reply_html(r: &crate::ReplyRef) -> String {
    let id = escape_attr(r.message_id.as_str());
    let mri = r
        .author
        .as_ref()
        .map(|a| escape_attr(a.as_str()))
        .unwrap_or_default();
    let name = escape_text(&r.author_name);
    let preview = escape_text(&truncate_chars(
        &r.preview.replace('\n', " "),
        REPLY_PREVIEW_CHARS,
    ));
    format!(
        r#"<blockquote itemscope="" itemtype="http://schema.skype.com/Reply" itemid="{id}"><strong itemprop="mri" itemid="{mri}">{name}</strong><span itemprop="time" itemid="{id}"></span><p itemprop="preview">{preview}</p></blockquote>"#
    )
}

/// Délka náhledu původní zprávy v odpovědi.
pub const REPLY_PREVIEW_CHARS: usize = 150;

/// Zkrátí text na `max` znaků, případně se „…“.
pub fn truncate_chars(s: &str, max: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= max {
        return s.to_owned();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out = out.trim_end().to_owned();
    out.push('…');
    out
}

fn escape_text(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn escape_attr(s: &str) -> String {
    escape_text(s).replace('"', "&quot;")
}

/// Velmi jednoduchý převod HTML na čitelný text (bez závislosti na HTML parseru).
pub fn html_to_plain(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut chars = html.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '<' => {
                let mut tag = String::new();
                for t in chars.by_ref() {
                    if t == '>' {
                        break;
                    }
                    tag.push(t);
                }
                let name: String = tag
                    .trim_start_matches('/')
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric())
                    .collect::<String>()
                    .to_ascii_lowercase();
                let closing = tag.starts_with('/');
                match name.as_str() {
                    "br" => out.push('\n'),
                    "p" | "div" | "li" | "blockquote" | "tr" if closing => out.push('\n'),
                    "li" => out.push_str("• "),
                    "img" => {
                        let alt = attr(&tag, "alt").unwrap_or_default();
                        out.push_str(&alt);
                    }
                    _ => {}
                }
            }
            '&' => {
                let mut entity = String::new();
                while let Some(&n) = chars.peek() {
                    if n == ';' || entity.len() > 10 || n.is_whitespace() || n == '&' || n == '<' {
                        break;
                    }
                    entity.push(n);
                    chars.next();
                }
                if chars.peek() == Some(&';') {
                    chars.next();
                    match decode_entity(&entity) {
                        Some(d) => out.push(d),
                        None => {
                            out.push('&');
                            out.push_str(&entity);
                            out.push(';');
                        }
                    }
                } else {
                    out.push('&');
                    out.push_str(&entity);
                }
            }
            c => out.push(c),
        }
    }
    // sloučit víc prázdných řádků a ořezat okraje
    let mut result = String::with_capacity(out.len());
    let mut blank = 0;
    for line in out.trim().lines() {
        let line = line.trim_end();
        if line.is_empty() {
            blank += 1;
            if blank > 1 {
                continue;
            }
        } else {
            blank = 0;
        }
        if !result.is_empty() {
            result.push('\n');
        }
        result.push_str(line);
    }
    result
}

/// Hodnota atributu `name="…"` ve značce (bez `<`/`>`), s dekódovanými entitami.
pub(crate) fn attr(tag: &str, name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    // mezera před názvem, aby `src` nenašlo `data-src` apod.
    let needle = format!(" {name}=\"");
    let start = lower.find(&needle)? + needle.len();
    let end = start + tag[start..].find('"')?;
    Some(html_to_plain(&tag[start..end]))
}

/// Část obsahu zprávy pro vykreslení.
#[derive(Debug, Clone, PartialEq)]
pub enum Part {
    Text(String),
    Image(InlineImage),
}

/// Obrázek vložený do zprávy (uložený v Microsoft media službě, vyžaduje přihlášení).
#[derive(Debug, Clone, PartialEq)]
pub struct InlineImage {
    pub url: String,
    pub width: Option<f32>,
    pub height: Option<f32>,
    pub alt: Option<String>,
}

impl InlineImage {
    /// URL originálu. Zpráva odkazuje na zmenšený náhled (`/views/imgo`);
    /// originál je ve view `imgpsh_fullsize_anim` (zachová i animaci GIFů).
    pub fn full_size_url(&self) -> String {
        match self.url.rfind("/views/") {
            Some(i) => format!("{}/views/imgpsh_fullsize_anim", &self.url[..i]),
            None => self.url.clone(),
        }
    }
}

/// Rozdělí HTML zprávy na text a vložené obrázky. Emoji (`<img alt="🙂">`) zůstávají v textu.
pub fn message_parts(html: &str) -> Vec<Part> {
    let mut parts = Vec::new();
    let mut chunk_start = 0;
    let mut cursor = 0;
    let push_text = |parts: &mut Vec<Part>, chunk: &str| {
        let t = html_to_plain(chunk);
        if !t.is_empty() {
            parts.push(Part::Text(t));
        }
    };
    while let Some(rel) = find_ascii_ci(&html[cursor..], "<img") {
        let pos = cursor + rel;
        let Some(len) = html[pos..].find('>') else {
            break;
        };
        let after = pos + len + 1;
        if let Some(img) = inline_image(&html[pos + 1..pos + len]) {
            push_text(&mut parts, &html[chunk_start..pos]);
            parts.push(Part::Image(img));
            chunk_start = after;
        }
        cursor = after;
    }
    push_text(&mut parts, &html[chunk_start..]);
    parts
}

pub(crate) fn inline_image(tag: &str) -> Option<InlineImage> {
    let url = attr(tag, "src")?;
    let ams = attr(tag, "itemtype").is_some_and(|t| t.contains("AMSImage"))
        || url::host(&url).is_some_and(|h| h.ends_with(".asm.skype.com"));
    if !ams || !url.starts_with("https://") {
        return None;
    }
    let num = |n: &str| {
        attr(tag, n)
            .and_then(|v| v.trim().parse::<f32>().ok())
            .filter(|v| *v > 0.0)
    };
    Some(InlineImage {
        width: num("width"),
        height: num("height"),
        alt: attr(tag, "alt").filter(|a| !a.is_empty()),
        url,
    })
}

pub(crate) fn find_ascii_ci(haystack: &str, needle: &str) -> Option<usize> {
    haystack
        .as_bytes()
        .windows(needle.len())
        .position(|w| w.eq_ignore_ascii_case(needle.as_bytes()))
}

mod url {
    /// Host z `https://host/...` bez závislosti na crate url.
    pub fn host(u: &str) -> Option<&str> {
        let rest = u.strip_prefix("https://")?;
        Some(rest.split(['/', '?', '#', ':']).next().unwrap_or(rest))
    }
}

pub(crate) fn decode_entity(e: &str) -> Option<char> {
    Some(match e {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        "nbsp" => ' ',
        _ => {
            let num = e.strip_prefix('#')?;
            let code = match num.strip_prefix(['x', 'X']) {
                Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                None => num.parse().ok()?,
            };
            char::from_u32(code)?
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_to_html_escapes() {
        assert_eq!(
            plain_to_html("a < b & \"c\"\ndruhý řádek\n"),
            "<p>a &lt; b &amp; &quot;c&quot;<br>druhý řádek</p>"
        );
    }

    #[test]
    fn splits_ams_images_and_keeps_emoji_inline() {
        let html = r#"<p>Koukni <img alt="🙂" itemtype="http://schema.skype.com/Emoji" src="https://statics.teams.cdn.office.net/e.png"> sem:</p><p><img src="https://eu-api.asm.skype.com/v1/objects/0-weu-d5-abc/views/imgo" itemtype="http://schema.skype.com/AMSImage" width="331.7" height="250" alt="obrázek"></p><p>a dál</p>"#;
        let parts = message_parts(html);
        assert_eq!(parts.len(), 3, "{parts:?}");
        assert_eq!(parts[0], Part::Text("Koukni 🙂 sem:".into()));
        match &parts[1] {
            Part::Image(i) => {
                assert_eq!(
                    i.url,
                    "https://eu-api.asm.skype.com/v1/objects/0-weu-d5-abc/views/imgo"
                );
                assert_eq!(i.width, Some(331.7));
                assert_eq!(i.height, Some(250.0));
                assert_eq!(i.alt.as_deref(), Some("obrázek"));
            }
            p => panic!("{p:?}"),
        }
        assert_eq!(parts[2], Part::Text("a dál".into()));
    }

    #[test]
    fn full_size_url_replaces_view() {
        let img = |url: &str| InlineImage {
            url: url.into(),
            width: None,
            height: None,
            alt: None,
        };
        assert_eq!(
            img("https://eu-api.asm.skype.com/v1/objects/0-weu-d5-abc/views/imgo").full_size_url(),
            "https://eu-api.asm.skype.com/v1/objects/0-weu-d5-abc/views/imgpsh_fullsize_anim"
        );
        assert_eq!(
            img("https://x.asm.skype.com/o").full_size_url(),
            "https://x.asm.skype.com/o"
        );
    }

    #[test]
    fn plain_message_is_single_text_part() {
        assert_eq!(
            message_parts("<p>Ahoj</p>"),
            vec![Part::Text("Ahoj".into())]
        );
        assert!(message_parts("<p></p>").is_empty());
        // http (ne https) obrázek se nezobrazí jako obrázek
        assert!(matches!(
            &message_parts(r#"<img src="http://eu-api.asm.skype.com/x" alt="x">"#)[..],
            [Part::Text(_)]
        ));
    }

    #[test]
    fn reply_html_is_escaped_and_parsed_back() {
        let r = crate::ReplyRef {
            message_id: crate::MessageId::from("1730000000000"),
            author: Some(crate::UserId::from("8:orgid:abc")),
            author_name: "Jan <Novák>".into(),
            preview: "a\nb & \"c\"".into(),
        };
        let html = reply_html(&r);
        assert!(html.contains(r#"itemid="1730000000000""#));
        let blocks = rich::parse(&format!("{html}<p>ok</p>"));
        match &blocks[0] {
            rich::Block::Quote(q) => {
                assert_eq!(
                    q.kind,
                    rich::QuoteKind::Reply {
                        message_id: Some("1730000000000".into()),
                        author: Some("Jan <Novák>".into()),
                        author_mri: Some("8:orgid:abc".into()),
                    }
                );
                assert_eq!(rich::plain(&q.blocks), "a b & \"c\"");
            }
            b => panic!("{b:?}"),
        }
        assert_eq!(truncate_chars("abcdef", 4), "abc…");
    }

    #[test]
    fn roundtrip() {
        let s = "if a < b && c > d\n'x'";
        assert_eq!(html_to_plain(&plain_to_html(s)), s);
    }

    #[test]
    fn html_to_plain_handles_structure_and_entities() {
        let html = "<p>Ahoj <b>světe</b>&nbsp;&#x1F600;</p><p></p><p></p><ul><li>jedna</li><li>dvě</li></ul>\
                    <img alt=\"(smile)\" src=\"x\"> A&B &unknown; &#999999999;";
        assert_eq!(
            html_to_plain(html),
            "Ahoj světe 😀\n\n• jedna\n• dvě\n(smile) A&B &unknown; &#999999999;"
        );
    }
}
