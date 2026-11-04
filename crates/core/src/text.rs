//! Převody mezi prostým textem a HTML obsahem zpráv.
//!
//! Plnohodnotné vykreslení rich textu bude v UI; tady je jen to, co potřebuje
//! odesílání, CLI a náhledy v seznamu chatů.

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

fn attr(tag: &str, name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let start = lower.find(&format!("{name}=\""))? + name.len() + 2;
    let end = start + tag[start..].find('"')?;
    Some(html_to_plain(&tag[start..end]))
}

fn decode_entity(e: &str) -> Option<char> {
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
