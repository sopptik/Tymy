//! @zmínky v psacím poli: rozpoznání rozepsané zmínky u kurzoru a výběr lidí.

/// Nejdelší dotaz za „@“, při kterém ještě nabízíme lidi.
const MAX_QUERY_CHARS: usize = 40;

/// Rozepsaná zmínka: bajtová pozice „@“ v řádku a text za ním až po kurzor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Query {
    pub line: usize,
    pub at: usize,
    pub text: String,
}

/// Najde rozepsanou zmínku v `line_text` před kurzorem (`col` = bajtový index).
/// „@“ musí být na začátku nebo za mezerou či závorkou (ne v e-mailu).
pub fn query_at(line: usize, line_text: &str, col: usize) -> Option<Query> {
    let col = col.min(line_text.len());
    let before = line_text.get(..col)?;
    let at = before.rfind('@')?;
    let prev = before[..at].chars().last();
    if prev.is_some_and(|p| !(p.is_whitespace() || "([{\"'".contains(p))) {
        return None;
    }
    let text = &before[at + 1..];
    if text.starts_with(' ') || text.contains("  ") || text.chars().count() > MAX_QUERY_CHARS {
        return None;
    }
    Some(Query {
        line,
        at,
        text: text.to_owned(),
    })
}

/// Odpovídá jméno dotazu? Každé slovo dotazu musí být začátkem některého slova jména;
/// bez ohledu na velikost písmen a diakritiku („novot“ najde „Novotný“).
pub fn matches(name: &str, query: &str) -> bool {
    let name = fold(name);
    let words: Vec<&str> = name.split_whitespace().collect();
    fold(query)
        .split_whitespace()
        .all(|q| words.iter().any(|w| w.starts_with(q)))
}

/// Malá písmena bez diakritiky (čeština, slovenština a běžné evropské znaky).
pub fn fold(s: &str) -> String {
    s.chars()
        .flat_map(char::to_lowercase)
        .map(|c| match c {
            'á' | 'à' | 'â' | 'ä' | 'ã' | 'å' | 'ą' => 'a',
            'č' | 'ć' | 'ç' => 'c',
            'ď' => 'd',
            'é' | 'ě' | 'è' | 'ê' | 'ë' | 'ę' => 'e',
            'í' | 'ì' | 'î' | 'ï' => 'i',
            'ľ' | 'ĺ' | 'ł' => 'l',
            'ň' | 'ń' | 'ñ' => 'n',
            'ó' | 'ò' | 'ô' | 'ö' | 'õ' | 'ő' => 'o',
            'ř' | 'ŕ' => 'r',
            'š' | 'ś' => 's',
            'ť' => 't',
            'ú' | 'ů' | 'ù' | 'û' | 'ü' | 'ű' => 'u',
            'ý' | 'ÿ' => 'y',
            'ž' | 'ź' | 'ż' => 'z',
            c => c,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_query_before_cursor() {
        let q = |s: &str| query_at(0, s, s.len()).map(|q| q.text);
        assert_eq!(q("ahoj @Jan"), Some("Jan".into()));
        assert_eq!(q("@"), Some(String::new()));
        assert_eq!(q("ahoj @Jan No"), Some("Jan No".into()));
        assert_eq!(q("(@ja"), Some("ja".into()));
        assert_eq!(q("mail@firma"), None, "e-mail není zmínka");
        assert_eq!(q("@ Jan"), None);
        assert_eq!(q("@Jan  No"), None);
        assert_eq!(q("bez zavináče"), None);
        // kurzor uprostřed: počítá se jen text před ním
        let s = "@Petr ahoj";
        assert_eq!(
            query_at(2, s, 3),
            Some(Query {
                line: 2,
                at: 0,
                text: "Pe".into()
            })
        );
        // bajtový index za „ř“
        let s = "@Jiř";
        assert_eq!(query_at(0, s, s.len()).unwrap().text, "Jiř");
    }

    #[test]
    fn matches_word_prefixes_without_diacritics() {
        assert!(matches("Jan Novotný", ""));
        assert!(matches("Jan Novotný", "novot"));
        assert!(matches("Jan Novotný", "NOVOTNÝ"));
        assert!(matches("Jan Novotný", "ja no"));
        assert!(matches("Řehoř Čížek", "rehor ciz"));
        assert!(!matches("Jan Novotný", "ovot"));
        assert!(!matches("Jan Novotný", "petr"));
    }
}
