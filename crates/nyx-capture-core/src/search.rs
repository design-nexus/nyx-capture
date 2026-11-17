//! FTS5 MATCH from typed text. User text is quoted, never concatenated into SQL.

/// Quote each word and join them with AND.
pub fn fts_match(input: &str) -> Option<String> {
    let mut terms = Vec::new();
    for raw in input.split_whitespace() {
        let word: String = raw.chars().filter(|c| !matches!(c, '"' | '*' | ':' | '(' | ')' | '^')).collect();
        if word.is_empty() {
            continue;
        }
        terms.push(format!("\"{}\"", word.replace('"', "\"\"")));
    }
    if terms.is_empty() { None } else { Some(terms.join(" AND ")) }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Recent,
    Screenshots,
    Recordings,
    Starred,
}

pub struct LibraryQuery {
    pub view: View,
    pub text: String,
}

/// `capture`, `capture starred`, or free text.
pub fn library_query(raw: &str) -> LibraryQuery {
    let trimmed = raw.trim();
    let lower = trimmed.to_ascii_lowercase();
    if lower.is_empty() || lower == "capture" {
        return LibraryQuery { view: View::Recent, text: String::new() };
    }
    for (prefix, view) in
        [("capture starred", View::Starred), ("capture screenshots", View::Screenshots), ("capture recordings", View::Recordings)]
    {
        if lower == prefix {
            return LibraryQuery { view, text: String::new() };
        }
        let with_space = format!("{prefix} ");
        if lower.starts_with(&with_space) {
            return LibraryQuery { view, text: trimmed[with_space.len()..].trim().to_string() };
        }
    }
    LibraryQuery { view: View::Recent, text: trimmed.to_string() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_user_text() {
        assert_eq!(fts_match("hello world").as_deref(), Some("\"hello\" AND \"world\""));
        assert_eq!(fts_match("a\"b"), Some("\"ab\"".into()));
        assert_eq!(fts_match("   "), None);
    }

    #[test]
    fn capture_commands() {
        let q = library_query("capture starred error");
        assert_eq!(q.view, View::Starred);
        assert_eq!(q.text, "error");
        assert_eq!(library_query("kernel").text, "kernel");
        assert_eq!(library_query("capture screenshots").view, View::Screenshots);
    }
}
