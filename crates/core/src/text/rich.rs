//! Strukturovaný obsah zprávy: HTML Teams → bloky (odstavce, seznamy, citace, kód, tabulky,
//! obrázky) s formátovanými úseky textu (tučně, odkazy, zmínky…). Slouží k vykreslení v okně.
//!
//! Parser je záměrně jednoduchý a tolerantní: neznámé značky ignoruje a zachová jejich text,
//! neuzavřené značky uzavře na konci.

use super::{InlineImage, attr, decode_entity, inline_image};

/// Formátování úseku textu.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Style {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strike: bool,
    /// Kód v řádku (`<code>`).
    pub code: bool,
    /// Zmínka osoby nebo kanálu (`@Jméno`).
    pub mention: bool,
}

/// Souvislý úsek textu se stejným formátováním.
#[derive(Debug, Clone, PartialEq)]
pub struct Span {
    pub text: String,
    pub style: Style,
    /// Cíl odkazu (jen `http(s)://` a `mailto:`).
    pub link: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Block {
    Paragraph(Vec<Span>),
    Heading(Vec<Span>),
    /// Položka seznamu; `depth` od 1, `marker` „•“ nebo „1.“ (prázdný u pokračování položky).
    ListItem {
        depth: usize,
        marker: String,
        spans: Vec<Span>,
    },
    Code {
        language: Option<String>,
        text: String,
    },
    Quote(Quote),
    Table(Vec<Vec<Cell>>),
    Image(InlineImage),
    /// Vodorovná čára (`<hr>`).
    Rule,
    /// Prázdný řádek (`<p>&nbsp;</p>`).
    Blank,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Cell {
    pub header: bool,
    pub spans: Vec<Span>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Quote {
    pub kind: QuoteKind,
    pub blocks: Vec<Block>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub enum QuoteKind {
    /// Obyčejná citace (`<blockquote>`).
    #[default]
    Plain,
    /// Přeposlaná zpráva.
    Forward,
    /// Odpověď na zprávu: obsah citace je zkrácený náhled původní zprávy.
    Reply {
        /// Id původní zprávy (v Teams je to čas jejího doručení v ms).
        message_id: Option<String>,
        author: Option<String>,
        author_mri: Option<String>,
    },
}

/// Převede obsah zprávy na bloky. Obsah bez HTML značek se bere jako prostý text
/// (zachovají se konce řádků).
pub fn parse(content: &str) -> Vec<Block> {
    let html = looks_like_html(content);
    let mut b = Builder::new(html);
    for token in Tokens::new(content) {
        match token {
            Token::Open { name, tag } => b.open(&name, tag),
            Token::Close(name) => b.close(&name),
            Token::Text(t) => b.text(&t),
        }
    }
    b.finish()
}

/// Text bloků pro kopírování. Citace odpovědi (náhled cizí zprávy) se vynechá,
/// ostatní citace dostanou předponu `> `.
pub fn plain(blocks: &[Block]) -> String {
    let mut lines: Vec<String> = Vec::new();
    for block in blocks {
        match block {
            Block::Paragraph(s) | Block::Heading(s) => lines.push(spans_text(s)),
            Block::ListItem {
                depth,
                marker,
                spans,
            } => {
                let indent = "  ".repeat(depth.saturating_sub(1));
                let marker = if marker.is_empty() {
                    "  ".to_owned()
                } else {
                    format!("{marker} ")
                };
                lines.push(format!("{indent}{marker}{}", spans_text(spans)));
            }
            Block::Code { text, .. } => lines.push(text.clone()),
            Block::Quote(q) => match q.kind {
                QuoteKind::Reply { .. } => {}
                _ => lines.extend(plain(&q.blocks).lines().map(|l| format!("> {l}"))),
            },
            Block::Table(rows) => {
                for row in rows {
                    let cells: Vec<String> = row.iter().map(|c| spans_text(&c.spans)).collect();
                    lines.push(cells.join("\t"));
                }
            }
            Block::Image(_) => {}
            Block::Rule => lines.push("———".into()),
            Block::Blank => lines.push(String::new()),
        }
    }
    lines.join("\n").trim().to_owned()
}

/// Spojený text úseků.
pub fn spans_text(spans: &[Span]) -> String {
    spans.iter().map(|s| s.text.as_str()).collect()
}

/// Má obsah nějakou HTML značku (`<p`, `</`, `<br` …)?
fn looks_like_html(s: &str) -> bool {
    s.as_bytes()
        .windows(2)
        .any(|w| w[0] == b'<' && (w[1].is_ascii_alphabetic() || w[1] == b'/'))
}

// ---------- tokenizer ----------

enum Token<'a> {
    /// Otevírací značka: malým písmem jméno a celý obsah značky (pro atributy).
    Open {
        name: String,
        tag: &'a str,
    },
    Close(String),
    Text(String),
}

struct Tokens<'a> {
    rest: &'a str,
    html: bool,
}

impl<'a> Tokens<'a> {
    fn new(s: &'a str) -> Self {
        Self {
            rest: s,
            html: looks_like_html(s),
        }
    }
}

impl<'a> Iterator for Tokens<'a> {
    type Item = Token<'a>;

    fn next(&mut self) -> Option<Token<'a>> {
        loop {
            if self.rest.is_empty() {
                return None;
            }
            if !self.html {
                let t = decode(self.rest);
                self.rest = "";
                return Some(Token::Text(t));
            }
            if let Some(after) = self.rest.strip_prefix("<!--") {
                self.rest = after.find("-->").map_or("", |i| &after[i + 3..]);
                continue;
            }
            if self.rest.starts_with('<') {
                let Some(end) = self.rest.find('>') else {
                    // neuzavřená značka: zbytek jako text
                    let t = decode(self.rest);
                    self.rest = "";
                    return Some(Token::Text(t));
                };
                let tag = &self.rest[1..end];
                self.rest = &self.rest[end + 1..];
                let closing = tag.starts_with('/');
                let name: String = tag
                    .trim_start_matches('/')
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric())
                    .collect::<String>()
                    .to_ascii_lowercase();
                if name.is_empty() {
                    continue; // `<!DOCTYPE>`, `<?xml>` apod.
                }
                return Some(if closing {
                    Token::Close(name)
                } else {
                    Token::Open { name, tag }
                });
            }
            let end = self.rest.find('<').unwrap_or(self.rest.len());
            let t = decode(&self.rest[..end]);
            self.rest = &self.rest[end..];
            return Some(Token::Text(t));
        }
    }
}

/// Dekóduje entity; `&nbsp;` zachová jako nezlomitelnou mezeru (neslučuje se).
fn decode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let end = rest[1..]
            .find(|c: char| c == ';' || c == '&' || c == '<' || c.is_whitespace())
            .map(|e| e + 1);
        match end {
            Some(e) if rest.as_bytes()[e] == b';' && e <= 12 => {
                let entity = &rest[1..e];
                let decoded = if entity == "nbsp" {
                    Some('\u{a0}')
                } else {
                    decode_entity(entity)
                };
                match decoded {
                    Some(c) => out.push(c),
                    None => out.push_str(&rest[..=e]),
                }
                rest = &rest[e + 1..];
            }
            _ => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

// ---------- stavba bloků ----------

/// Co se má stát při uzavření značky.
enum Effect {
    None,
    /// Hranice bloku (`p`, `div`).
    Block,
    Style(Style),
    Link(Option<String>),
    Skip,
    Author,
    Heading,
    List,
    Item,
    Quote,
    Pre,
    Table,
    Row,
    Cell,
}

struct Frame {
    kind: QuoteKind,
    blocks: Vec<Block>,
}

#[derive(Default)]
struct Table {
    rows: Vec<Vec<Cell>>,
    row: Option<Vec<Cell>>,
    cell: Option<Cell>,
}

struct Builder {
    html: bool,
    frames: Vec<Frame>,
    inline: Vec<Span>,
    style: Style,
    link: Option<String>,
    open: Vec<(String, Effect)>,
    /// Otevřené seznamy: `None` = odrážky, `Some(n)` = číslovaný (poslední číslo).
    lists: Vec<Option<u32>>,
    item: Option<(usize, String)>,
    heading: bool,
    pre: Option<(Option<String>, String)>,
    table: Option<Table>,
    author: Option<String>,
    skip: usize,
}

const VOID: &[&str] = &[
    "br", "img", "hr", "input", "meta", "link", "wbr", "col", "source", "area", "base",
];

impl Builder {
    fn new(html: bool) -> Self {
        Self {
            html,
            frames: vec![Frame {
                kind: QuoteKind::Plain,
                blocks: Vec::new(),
            }],
            inline: Vec::new(),
            style: Style::default(),
            link: None,
            open: Vec::new(),
            lists: Vec::new(),
            item: None,
            heading: false,
            pre: None,
            table: None,
            author: None,
            skip: 0,
        }
    }

    fn frame(&mut self) -> &mut Frame {
        self.frames.last_mut().expect("kořen se nikdy neodebírá")
    }

    fn open(&mut self, name: &str, tag: &str) {
        let void = VOID.contains(&name) || tag.trim_end().ends_with('/');
        if self.skip > 0 {
            if !void {
                self.skip += 1;
                self.open.push((name.to_owned(), Effect::Skip));
            }
            return;
        }
        match name {
            "br" => {
                if let Some((_, code)) = &mut self.pre {
                    code.push('\n');
                } else {
                    self.push_raw("\n");
                }
                return;
            }
            "img" => {
                match inline_image(tag) {
                    Some(img) if self.pre.is_none() && self.table.is_none() => {
                        self.flush();
                        self.frame().blocks.push(Block::Image(img));
                    }
                    _ => {
                        // emoji a ostatní obrázky jako text (alt)
                        if let Some(alt) = attr(tag, "alt") {
                            self.text_raw(&alt);
                        }
                    }
                }
                return;
            }
            "hr" => {
                self.flush();
                self.frame().blocks.push(Block::Rule);
                return;
            }
            _ if void => return,
            _ => {}
        }
        let effect = match name {
            "p" | "div" | "section" | "article" | "header" | "footer" => {
                // nadpis bloku kódu, který editor Teams vkládá před `<pre>`
                if attr(tag, "itemtype").is_some_and(|t| t.contains("CodeBlockEditor")) {
                    self.skip += 1;
                    Effect::Skip
                } else {
                    self.flush();
                    Effect::Block
                }
            }
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                self.flush();
                self.heading = true;
                Effect::Heading
            }
            "ul" | "ol" => {
                self.flush();
                self.lists.push((name == "ol").then_some(0));
                Effect::List
            }
            "li" => {
                self.flush();
                let depth = self.lists.len().max(1);
                let marker = match self.lists.last_mut() {
                    Some(Some(n)) => {
                        *n += 1;
                        format!("{n}.")
                    }
                    _ => "•".to_owned(),
                };
                self.item = Some((depth, marker));
                Effect::Item
            }
            "blockquote" => {
                self.flush();
                let itemtype = attr(tag, "itemtype").unwrap_or_default();
                let kind = if itemtype.contains("schema.skype.com/Reply") {
                    QuoteKind::Reply {
                        message_id: attr(tag, "itemid").filter(|i| !i.is_empty()),
                        author: None,
                        author_mri: None,
                    }
                } else if itemtype.contains("schema.skype.com/Forward") {
                    QuoteKind::Forward
                } else {
                    QuoteKind::Plain
                };
                self.frames.push(Frame {
                    kind,
                    blocks: Vec::new(),
                });
                Effect::Quote
            }
            "pre" => {
                self.flush();
                let language = attr(tag, "class").and_then(|c| {
                    c.split_whitespace()
                        .filter_map(|c| c.strip_prefix("language-"))
                        .find(|l| *l != "was-manually-selected" && *l != "plaintext")
                        .map(str::to_owned)
                });
                self.pre = Some((language, String::new()));
                Effect::Pre
            }
            "table" => {
                self.flush();
                self.table = Some(Table::default());
                Effect::Table
            }
            "tr" => match &mut self.table {
                Some(t) => {
                    finish_row(t);
                    t.row = Some(Vec::new());
                    Effect::Row
                }
                None => Effect::None,
            },
            "td" | "th" => match &mut self.table {
                Some(t) => {
                    finish_cell(t);
                    t.cell = Some(Cell {
                        header: name == "th",
                        spans: Vec::new(),
                    });
                    Effect::Cell
                }
                None => Effect::None,
            },
            "strong" | "b"
                if attr(tag, "itemprop").as_deref() == Some("mri")
                    && matches!(self.frame().kind, QuoteKind::Reply { .. }) =>
            {
                if let QuoteKind::Reply { author_mri, .. } = &mut self.frame().kind {
                    *author_mri = attr(tag, "itemid").filter(|i| !i.is_empty());
                }
                self.author = Some(String::new());
                Effect::Author
            }
            "strong" | "b" => self.restyle(|s| s.bold = true),
            "i" | "em" => self.restyle(|s| s.italic = true),
            "u" | "ins" => self.restyle(|s| s.underline = true),
            "s" | "strike" | "del" => self.restyle(|s| s.strike = true),
            "code" if self.pre.is_none() => self.restyle(|s| s.code = true),
            "a" => {
                let prev = self.link.clone();
                if let Some(href) = attr(tag, "href").filter(|h| safe_link(h)) {
                    self.link = Some(href);
                }
                Effect::Link(prev)
            }
            "span" => {
                let itemprop = attr(tag, "itemprop").unwrap_or_default();
                if itemprop == "time" || itemprop == "emojis" {
                    self.skip += 1;
                    Effect::Skip
                } else if attr(tag, "itemtype").is_some_and(|t| t.contains("Mention")) {
                    self.restyle(|s| s.mention = true)
                } else {
                    Effect::None
                }
            }
            "style" | "script" | "head" | "title" | "template" => {
                self.skip += 1;
                Effect::Skip
            }
            _ => Effect::None,
        };
        self.open.push((name.to_owned(), effect));
    }

    fn restyle(&mut self, f: impl FnOnce(&mut Style)) -> Effect {
        let prev = self.style;
        f(&mut self.style);
        Effect::Style(prev)
    }

    fn close(&mut self, name: &str) {
        let Some(idx) = self.open.iter().rposition(|(n, _)| n == name) else {
            return;
        };
        while self.open.len() > idx {
            let (_, effect) = self.open.pop().expect("délka ověřena");
            self.end(effect);
        }
    }

    fn end(&mut self, effect: Effect) {
        match effect {
            Effect::None => {}
            Effect::Block => self.flush(),
            Effect::Style(prev) => self.style = prev,
            Effect::Link(prev) => self.link = prev,
            Effect::Skip => self.skip = self.skip.saturating_sub(1),
            Effect::Author => {
                let name = self.author.take().map(|a| a.trim().to_owned());
                if let QuoteKind::Reply { author, .. } = &mut self.frame().kind {
                    *author = name.filter(|n| !n.is_empty());
                }
            }
            Effect::Heading => {
                self.flush();
                self.heading = false;
            }
            Effect::Item => {
                self.flush();
                self.item = None;
            }
            Effect::List => {
                self.flush();
                self.lists.pop();
            }
            Effect::Quote => {
                self.flush();
                if self.frames.len() > 1 {
                    let f = self.frames.pop().expect("délka ověřena");
                    let blocks = tidy(f.blocks);
                    let empty_reply =
                        blocks.is_empty() && !matches!(f.kind, QuoteKind::Reply { .. });
                    if !empty_reply {
                        self.frame().blocks.push(Block::Quote(Quote {
                            kind: f.kind,
                            blocks,
                        }));
                    }
                }
            }
            Effect::Pre => {
                if let Some((language, code)) = self.pre.take() {
                    let text = code.replace('\u{a0}', " ").trim_end().to_owned();
                    let text = text.trim_start_matches('\n').to_owned();
                    if !text.is_empty() {
                        self.frame().blocks.push(Block::Code { language, text });
                    }
                }
            }
            Effect::Table => {
                if let Some(mut t) = self.table.take() {
                    finish_row(&mut t);
                    t.rows.retain(|r| !r.is_empty());
                    if !t.rows.is_empty() {
                        self.frame().blocks.push(Block::Table(t.rows));
                    }
                }
            }
            Effect::Row => {
                if let Some(t) = &mut self.table {
                    finish_row(t);
                }
            }
            Effect::Cell => {
                if let Some(t) = &mut self.table {
                    finish_cell(t);
                }
            }
        }
    }

    /// Textový uzel z HTML (v HTML režimu se bílé znaky slučují jako v prohlížeči).
    fn text(&mut self, t: &str) {
        if self.skip > 0 {
            return;
        }
        if let Some(a) = &mut self.author {
            a.push_str(t);
            return;
        }
        if let Some((_, code)) = &mut self.pre {
            code.push_str(t);
            return;
        }
        if self.html {
            let collapsed = collapse_ws(t);
            self.text_raw(&collapsed);
        } else {
            self.text_raw(t);
        }
    }

    /// Přidá text do rozpracovaného odstavce (nebo buňky tabulky).
    fn text_raw(&mut self, t: &str) {
        if self.skip > 0 {
            return;
        }
        if let Some(a) = &mut self.author {
            a.push_str(t);
            return;
        }
        if let Some((_, code)) = &mut self.pre {
            code.push_str(t);
            return;
        }
        let (style, link) = (self.style, self.link.clone());
        let target = match &mut self.table {
            Some(Table { cell: Some(c), .. }) => &mut c.spans,
            Some(_) => return, // text mezi buňkami (bílé znaky)
            None => &mut self.inline,
        };
        // na začátku řádku mezery nepřidávat
        let at_line_start = target.last().is_none_or(|s| s.text.ends_with(['\n', ' ']));
        let t = if at_line_start {
            t.trim_start_matches(' ')
        } else {
            t
        };
        if t.is_empty() {
            return;
        }
        push_span(target, t, style, link);
    }

    /// Konec řádku (`<br>`); na začátku odstavce se ignoruje.
    fn push_raw(&mut self, t: &str) {
        let empty = match &self.table {
            Some(Table { cell: Some(c), .. }) => c.spans.is_empty(),
            Some(_) => true,
            None => self.inline.is_empty(),
        };
        if !empty {
            let (style, link) = (self.style, self.link.clone());
            match &mut self.table {
                Some(Table { cell: Some(c), .. }) => push_span(&mut c.spans, t, style, link),
                _ => push_span(&mut self.inline, t, style, link),
            }
        }
    }

    /// Uzavře rozpracovaný odstavec do bloku.
    fn flush(&mut self) {
        if let Some(t) = &mut self.table {
            // odstavec uvnitř buňky = nový řádek v buňce
            if let Some(c) = &mut t.cell
                && c.spans.last().is_some_and(|s| !s.text.ends_with('\n'))
            {
                let (style, link) = (self.style, self.link.clone());
                push_span(&mut c.spans, "\n", style, link);
            }
            return;
        }
        if self.inline.is_empty() {
            return;
        }
        let mut spans = std::mem::take(&mut self.inline);
        trim_spans(&mut spans);
        let block = if spans.is_empty() {
            Block::Blank
        } else {
            let spans = autolink(spans);
            if self.heading {
                Block::Heading(spans)
            } else if let Some((depth, marker)) = &mut self.item {
                let b = Block::ListItem {
                    depth: *depth,
                    marker: marker.clone(),
                    spans,
                };
                marker.clear(); // další odstavec téže položky už bez značky
                b
            } else {
                Block::Paragraph(spans)
            }
        };
        self.frame().blocks.push(block);
    }

    fn finish(mut self) -> Vec<Block> {
        while let Some((_, effect)) = self.open.pop() {
            self.end(effect);
        }
        self.flush();
        while self.frames.len() > 1 {
            self.end(Effect::Quote);
        }
        let root = self.frames.pop().expect("kořen");
        tidy(root.blocks)
    }
}

fn finish_cell(t: &mut Table) {
    if let Some(mut cell) = t.cell.take() {
        trim_spans(&mut cell.spans);
        cell.spans = autolink(cell.spans);
        t.row.get_or_insert_with(Vec::new).push(cell);
    }
}

fn finish_row(t: &mut Table) {
    finish_cell(t);
    if let Some(row) = t.row.take() {
        t.rows.push(row);
    }
}

fn push_span(target: &mut Vec<Span>, t: &str, style: Style, link: Option<String>) {
    match target.last_mut() {
        Some(last) if last.style == style && last.link == link => last.text.push_str(t),
        _ => target.push(Span {
            text: t.to_owned(),
            style,
            link,
        }),
    }
}

/// Ořízne bílé znaky na začátku a konci odstavce; prázdné úseky zahodí.
/// Nezlomitelné mezery převede na obyčejné.
fn trim_spans(spans: &mut Vec<Span>) {
    for s in spans.iter_mut() {
        if s.text.contains('\u{a0}') {
            s.text = s.text.replace('\u{a0}', " ");
        }
    }
    while let Some(first) = spans.first_mut() {
        let t = first.text.trim_start();
        if t.is_empty() {
            spans.remove(0);
        } else {
            first.text = t.to_owned();
            break;
        }
    }
    while let Some(last) = spans.last_mut() {
        let t = last.text.trim_end();
        if t.is_empty() {
            spans.pop();
        } else {
            last.text = t.to_owned();
            break;
        }
    }
}

/// Bílé znaky (mimo nezlomitelné mezery) sloučí do jedné mezery.
fn collapse_ws(t: &str) -> String {
    let mut out = String::with_capacity(t.len());
    let mut ws = false;
    for c in t.chars() {
        if matches!(c, ' ' | '\t' | '\n' | '\r' | '\u{c}') {
            if !ws {
                out.push(' ');
            }
            ws = true;
        } else {
            out.push(c);
            ws = false;
        }
    }
    out
}

/// Odstraní prázdné řádky na začátku a konci a sloučí opakované.
fn tidy(blocks: Vec<Block>) -> Vec<Block> {
    let mut out: Vec<Block> = Vec::with_capacity(blocks.len());
    for b in blocks {
        if b == Block::Blank && out.last().is_none_or(|l| *l == Block::Blank) {
            continue;
        }
        out.push(b);
    }
    while out.last() == Some(&Block::Blank) {
        out.pop();
    }
    out
}

/// Povolené cíle odkazů: web a e-mail (žádné `file:`, `javascript:` ani vlastní schémata).
pub fn safe_link(href: &str) -> bool {
    let lower = href.trim().to_ascii_lowercase();
    ["https://", "http://", "mailto:"]
        .iter()
        .any(|p| lower.starts_with(p) && lower.len() > p.len())
}

/// Najde v textu holé adresy (`https://…`, `www.…`) a udělá z nich odkazy.
fn autolink(spans: Vec<Span>) -> Vec<Span> {
    let mut out = Vec::with_capacity(spans.len());
    for span in spans {
        if span.link.is_some() || span.style.code {
            out.push(span);
            continue;
        }
        let mut rest = span.text.as_str();
        while let Some((start, end)) = find_url(rest) {
            if start > 0 {
                out.push(Span {
                    text: rest[..start].to_owned(),
                    style: span.style,
                    link: None,
                });
            }
            let url = &rest[start..end];
            let href = if url.starts_with("www.") {
                format!("https://{url}")
            } else {
                url.to_owned()
            };
            out.push(Span {
                text: url.to_owned(),
                style: span.style,
                link: Some(href),
            });
            rest = &rest[end..];
        }
        if !rest.is_empty() {
            out.push(Span {
                text: rest.to_owned(),
                style: span.style,
                link: None,
            });
        }
    }
    out
}

/// Pozice první adresy v textu (začátek, konec v bajtech).
fn find_url(s: &str) -> Option<(usize, usize)> {
    let lower = s.to_ascii_lowercase();
    let mut from = 0;
    loop {
        let start = ["https://", "http://", "www."]
            .iter()
            .filter_map(|p| lower[from..].find(p).map(|i| (from + i, p.len())))
            .min_by_key(|(i, _)| *i)?;
        let (i, plen) = start;
        // adresa musí začínat na hranici slova
        let boundary = s[..i]
            .chars()
            .next_back()
            .is_none_or(|c| !(c.is_alphanumeric() || matches!(c, '.' | '/' | '@' | '-')));
        let mut end = s[i..]
            .find(|c: char| c.is_whitespace() || matches!(c, '<' | '>' | '"' | '`'))
            .map_or(s.len(), |e| i + e);
        // koncová interpunkce do adresy nepatří
        while end > i {
            let last = s[..end].chars().next_back().expect("neprázdné");
            let unbalanced = match last {
                ')' => s[i..end].matches('(').count() < s[i..end].matches(')').count(),
                ']' => s[i..end].matches('[').count() < s[i..end].matches(']').count(),
                _ => false,
            };
            if matches!(last, '.' | ',' | ';' | ':' | '!' | '?' | '\'' | '"') || unbalanced {
                end -= last.len_utf8();
            } else {
                break;
            }
        }
        if boundary && end > i + plen {
            return Some((i, end));
        }
        from = i + plen;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(text: &str) -> Span {
        Span {
            text: text.into(),
            style: Style::default(),
            link: None,
        }
    }

    fn styled(text: &str, f: impl FnOnce(&mut Style)) -> Span {
        let mut style = Style::default();
        f(&mut style);
        Span {
            text: text.into(),
            style,
            link: None,
        }
    }

    fn link(text: &str, href: &str) -> Span {
        Span {
            text: text.into(),
            style: Style::default(),
            link: Some(href.into()),
        }
    }

    #[test]
    fn plain_text_keeps_newlines_and_links() {
        assert_eq!(
            parse("ahoj\nviz https://example.com/a?b=1."),
            vec![Block::Paragraph(vec![
                p("ahoj\nviz "),
                link("https://example.com/a?b=1", "https://example.com/a?b=1"),
                p("."),
            ])]
        );
    }

    #[test]
    fn paragraphs_formatting_and_whitespace() {
        let html = "<p>Ahoj <b>tučně</b> a <i>kurzíva</i>,\n <s>ne</s> <u>pod</u> <code>x()</code></p>\r\n\r\n<p>&nbsp;</p>\r\n<p>&nbsp;</p><p>druhý<br>řádek<br></p>\r\n";
        assert_eq!(
            parse(html),
            vec![
                Block::Paragraph(vec![
                    p("Ahoj "),
                    styled("tučně", |s| s.bold = true),
                    p(" a "),
                    styled("kurzíva", |s| s.italic = true),
                    p(", "),
                    styled("ne", |s| s.strike = true),
                    p(" "),
                    styled("pod", |s| s.underline = true),
                    p(" "),
                    styled("x()", |s| s.code = true),
                ]),
                Block::Blank,
                Block::Paragraph(vec![p("druhý\nřádek")]),
            ]
        );
    }

    #[test]
    fn reply_quote() {
        let html = r#"<blockquote itemscope itemtype="http://schema.skype.com/Reply" itemid="1730000000000"><strong itemprop="mri" itemid="8:orgid:abc">Jan Novák</strong><span itemprop="time" itemid="1730000000000"></span><p itemprop="preview">Původní zpráva…</p><span itemprop="emojis"><span itemprop="smile" itemid="smile" alt="x" itemtype="false"></span></span></blockquote>
<p>Moje odpověď</p>"#;
        assert_eq!(
            parse(html),
            vec![
                Block::Quote(Quote {
                    kind: QuoteKind::Reply {
                        message_id: Some("1730000000000".into()),
                        author: Some("Jan Novák".into()),
                        author_mri: Some("8:orgid:abc".into()),
                    },
                    blocks: vec![Block::Paragraph(vec![p("Původní zpráva…")])],
                }),
                Block::Paragraph(vec![p("Moje odpověď")]),
            ]
        );
        assert_eq!(plain(&parse(html)), "Moje odpověď");
    }

    #[test]
    fn mentions_links_and_emoji() {
        let html = r#"<p>Ahoj <span itemtype="http://schema.skype.com/Mention" itemscope="" itemid="0">Jano</span>, koukni <a href="https://example.com/x" rel="noreferrer noopener" target="_blank" title="https://example.com/x">sem</a> <span title="Smile" type="(smile)" class="animated-emoticon-20-smile" itemscope=""><img itemscope="" itemtype="http://schema.skype.com/Emoji" itemid="smile" src="https://statics.teams.cdn.office.net/x.png" title="Smile" alt="🙂" style="width:20px; height:20px"></span> <a href="javascript:alert(1)">zlý</a></p>"#;
        assert_eq!(
            parse(html),
            vec![Block::Paragraph(vec![
                p("Ahoj "),
                styled("Jano", |s| s.mention = true),
                p(", koukni "),
                link("sem", "https://example.com/x"),
                p(" 🙂 zlý"),
            ])]
        );
    }

    #[test]
    fn code_block_keeps_indentation_and_skips_editor_label() {
        let html = r#"<p>Kód:</p>
<p itemtype="http://schema.skype.com/CodeBlockEditor" id="x">&nbsp;</p>
<pre class="language-rust language-was-manually-selected" itemid="y"><code><span class="hljs-keyword">fn</span> main() {<br>&nbsp;&nbsp;&nbsp;&nbsp;x &lt; 1;<br>}</code></pre>"#;
        assert_eq!(
            parse(html),
            vec![
                Block::Paragraph(vec![p("Kód:")]),
                Block::Code {
                    language: Some("rust".into()),
                    text: "fn main() {\n    x < 1;\n}".into(),
                },
            ]
        );
    }

    #[test]
    fn lists_headings_rule_and_forward() {
        let html = "<h3>Nadpis</h3><ul><li>jedna</li><li><b>dvě</b><ol><li>a</li><li>b</li></ol></li></ul><hr><blockquote itemtype=\"http://schema.skype.com/Forward\">\r\n<p>přeposláno</p></blockquote><blockquote><p>citát</p></blockquote>";
        let item = |depth, marker: &str, spans| Block::ListItem {
            depth,
            marker: marker.into(),
            spans,
        };
        let blocks = parse(html);
        assert_eq!(
            blocks,
            vec![
                Block::Heading(vec![p("Nadpis")]),
                item(1, "•", vec![p("jedna")]),
                item(1, "•", vec![styled("dvě", |s| s.bold = true)]),
                item(2, "1.", vec![p("a")]),
                item(2, "2.", vec![p("b")]),
                Block::Rule,
                Block::Quote(Quote {
                    kind: QuoteKind::Forward,
                    blocks: vec![Block::Paragraph(vec![p("přeposláno")])],
                }),
                Block::Quote(Quote {
                    kind: QuoteKind::Plain,
                    blocks: vec![Block::Paragraph(vec![p("citát")])],
                }),
            ]
        );
        assert_eq!(
            plain(&blocks),
            "Nadpis\n• jedna\n• dvě\n  1. a\n  2. b\n———\n> přeposláno\n> citát"
        );
    }

    #[test]
    fn table_and_images() {
        let html = r#"<table>
<thead><tr><th>A</th><th>B</th></tr></thead>
<tbody><tr><td><p>1</p><p>2</p></td><td>x</td></tr></tbody></table><p><img src="https://eu-api.asm.skype.com/v1/objects/0-weu-d1-abc/views/imgo" itemtype="http://schema.skype.com/AMSImage" width="100" height="50" alt="obr"></p>"#;
        let blocks = parse(html);
        let cell = |header, text: &str| Cell {
            header,
            spans: vec![p(text)],
        };
        assert_eq!(
            blocks[0],
            Block::Table(vec![
                vec![cell(true, "A"), cell(true, "B")],
                vec![cell(false, "1\n2"), cell(false, "x")],
            ])
        );
        assert!(matches!(&blocks[1], Block::Image(i) if i.width == Some(100.0)));
        assert_eq!(blocks.len(), 2);
    }

    #[test]
    fn autolinks_only_at_word_boundary() {
        assert_eq!(find_url("(viz www.seznam.cz)"), Some((5, 18)));
        assert_eq!(
            find_url("x https://cs.wikipedia.org/wiki/A_(b)."),
            Some((2, 37))
        );
        assert_eq!(find_url("mail@www.example.com"), None);
        assert_eq!(find_url("https:// nic"), None);
        assert!(safe_link("mailto:a@b.cz"));
        assert!(!safe_link("file:///etc/passwd"));
    }

    #[test]
    fn tolerates_broken_html() {
        let blocks = parse("<p>a <b>b <i>c</p><p>d</b> e");
        assert_eq!(plain(&blocks), "a b c\nd e");
        assert_eq!(parse("<p></p>"), vec![]);
        assert_eq!(
            parse("a &amp b &lt; &#x1F600; &bogus;"),
            vec![Block::Paragraph(vec![p("a &amp b < 😀 &bogus;")])]
        );
    }
}

/// Ruční kontrola na skutečných zprávách:
/// `TYMY_RICH_DUMP=<soubor.json s polem obsahů> cargo test -p tymy-core real_messages -- --nocapture`.
#[cfg(test)]
#[test]
fn real_messages() {
    let Ok(path) = std::env::var("TYMY_RICH_DUMP") else {
        return;
    };
    let contents: Vec<String> =
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let (mut quotes, mut code, mut tables, mut links, mut leftovers) = (0, 0, 0, 0, 0);
    for c in &contents {
        let blocks = parse(c);
        let text = plain(
            &blocks
                .iter()
                .filter(|b| !matches!(b, Block::Code { .. }))
                .cloned()
                .collect::<Vec<_>>(),
        );
        // zbytky značek v textu = něco jsme nerozpoznali
        if text.contains("<p") || text.contains("</") || text.contains("<span") {
            leftovers += 1;
        }
        for b in &blocks {
            match b {
                Block::Quote(_) => quotes += 1,
                Block::Code { .. } => code += 1,
                Block::Table(_) => tables += 1,
                Block::Paragraph(s) if s.iter().any(|s| s.link.is_some()) => links += 1,
                _ => {}
            }
        }
    }
    println!(
        "zpráv {}, citace {quotes}, kód {code}, tabulky {tables}, odstavce s odkazem {links}, zbytky značek {leftovers}",
        contents.len()
    );
}
