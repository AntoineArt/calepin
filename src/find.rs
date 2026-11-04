//! Text search: case- and accent-insensitive, on character offsets.

/// One character folded for comparison. Always one char for one char, so
/// match offsets stay valid in the original text.
pub fn fold(c: char) -> char {
    let l = c.to_lowercase().next().unwrap_or(c);
    match l {
        'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' => 'a',
        'ç' => 'c',
        'è' | 'é' | 'ê' | 'ë' => 'e',
        'ì' | 'í' | 'î' | 'ï' => 'i',
        'ñ' => 'n',
        'ò' | 'ó' | 'ô' | 'õ' | 'ö' => 'o',
        'ù' | 'ú' | 'û' | 'ü' => 'u',
        'ý' | 'ÿ' => 'y',
        '’' | '‘' => '\'',
        '\u{a0}' | '\u{202f}' => ' ',
        _ => l,
    }
}

/// Character ranges of every non-overlapping occurrence of `needle`.
pub fn find_all(haystack: &str, needle: &str) -> Vec<(usize, usize)> {
    let n: Vec<char> = needle.chars().map(fold).collect();
    if n.is_empty() {
        return Vec::new();
    }
    let h: Vec<char> = haystack.chars().map(fold).collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i + n.len() <= h.len() {
        if h[i..i + n.len()] == n[..] {
            out.push((i, i + n.len()));
            i += n.len();
        } else {
            i += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insensitive() {
        assert_eq!(find_all("État et etat, ÉTAT", "etat"), vec![(0, 4), (8, 12), (14, 18)]);
        assert_eq!(find_all("aaaa", "aa"), vec![(0, 2), (2, 4)]);
        assert_eq!(find_all("Œuvre", "x"), vec![]);
        assert_eq!(find_all("l’API", "l'api"), vec![(0, 5)]);
        assert!(find_all("abc", "").is_empty());
    }
}
