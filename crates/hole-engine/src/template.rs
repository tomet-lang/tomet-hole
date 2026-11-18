//! Replacement template parsing and expansion for `hole-engine`.
//!
//! Templates embed captured bindings using `${hole.name}` (as specified in
//! `hole.tmt`) or shorthand `${name}`. Everything else, including `$` not
//! followed by `{`, is preserved literally.

use std::collections::BTreeMap;
use std::fmt;

use crate::vm::Match;

/// Namespace prefix supported inside `${hole.name}` placeholders.
const HOLE_NAMESPACE: &str = "hole.";

/// Error returned when parsing or expanding a replacement template fails.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemplateError {
    pub message: String,
}

impl TemplateError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for TemplateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for TemplateError {}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Piece {
    Text(String),
    Capture(String),
}

/// A compiled replacement template containing `${hole.name}` or `${name}` placeholders.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Template {
    pieces: Vec<Piece>,
}

impl Template {
    /// Parses a template string into literal segments and capture references.
    pub fn parse(src: &str) -> Result<Self, TemplateError> {
        let mut pieces = Vec::new();
        let mut rest = src;
        let mut offset = 0usize;

        while let Some(open) = rest.find("${") {
            if open > 0 {
                pieces.push(Piece::Text(rest[..open].to_string()));
            }
            let after_open = &rest[open + 2..];
            let Some(close) = after_open.find('}') else {
                return Err(TemplateError::new(format!(
                    "unclosed `${{` placeholder at byte {}",
                    offset + open
                )));
            };
            let expr = &after_open[..close];
            let name = parse_placeholder(expr).map_err(|msg| {
                TemplateError::new(format!("{msg} (at byte {})", offset + open))
            })?;
            pieces.push(Piece::Capture(name.to_string()));

            let advance = open + 2 + close + 1;
            rest = &rest[advance..];
            offset += advance;
        }

        if !rest.is_empty() {
            pieces.push(Piece::Text(rest.to_string()));
        }

        Ok(Self { pieces })
    }

    /// Returns the capture names referenced by this template, in order of appearance.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.pieces.iter().filter_map(|p| match p {
            Piece::Capture(name) => Some(name.as_str()),
            Piece::Text(_) => None,
        })
    }

    /// Expands the template against a [`Match`].
    pub fn expand(&self, matched: &Match) -> Result<String, TemplateError> {
        self.expand_with(|name| matched.get(name).map(|cap| cap.text.as_str()))
    }

    /// Expands the template against a map of named bindings.
    pub fn expand_map(&self, bindings: &BTreeMap<String, String>) -> Result<String, TemplateError> {
        self.expand_with(|name| bindings.get(name).map(String::as_str))
    }

    /// Expands the template using a custom capture lookup callback.
    pub fn expand_with<'a>(
        &self,
        mut lookup: impl FnMut(&str) -> Option<&'a str>,
    ) -> Result<String, TemplateError> {
        let mut out = String::new();
        for piece in &self.pieces {
            match piece {
                Piece::Text(text) => out.push_str(text),
                Piece::Capture(name) => match lookup(name) {
                    Some(value) => out.push_str(value),
                    None => {
                        return Err(TemplateError::new(format!(
                            "unknown capture `{name}` in template"
                        )));
                    }
                },
            }
        }
        Ok(out)
    }

    /// Parses `template` and immediately expands it against `matched`.
    pub fn expand_str(template: &str, matched: &Match) -> Result<String, TemplateError> {
        Self::parse(template)?.expand(matched)
    }
}

fn parse_placeholder(expr: &str) -> Result<&str, String> {
    let name = expr.strip_prefix(HOLE_NAMESPACE).unwrap_or(expr);
    if is_capture_name(name) {
        Ok(name)
    } else if expr.is_empty() {
        Err("empty `${}` placeholder".to_string())
    } else {
        Err(format!(
            "invalid placeholder `${{{expr}}}`: expected `${{hole.name}}` or `${{name}}`"
        ))
    }
}

fn is_capture_name(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_alphanumeric() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Pattern, Program};

    #[test]
    fn expands_hole_namespace_and_shorthand_names() {
        let pat = Pattern::seq(vec![
            Pattern::seg().capture(Some("p".into())),
            Pattern::lit("/"),
            Pattern::seg().capture(Some("n".into())),
        ]);
        let m = Program::compile(&pat).match_full("core/parser").unwrap();

        let t1 = Template::parse("tests/${hole.p}/${hole.n}_test.rs").unwrap();
        assert_eq!(t1.names().collect::<Vec<_>>(), ["p", "n"]);
        assert_eq!(t1.expand(&m).unwrap(), "tests/core/parser_test.rs");

        let t2 = Template::parse("tests/${p}/${n}_test.rs").unwrap();
        assert_eq!(t2.expand(&m).unwrap(), "tests/core/parser_test.rs");
    }

    #[test]
    fn does_not_reexpand_captured_values_or_confuse_prefixes() {
        let mut map = BTreeMap::new();
        map.insert("k".to_string(), "${hole.kind}".to_string());
        map.insert("kind".to_string(), "article".to_string());

        let tpl = Template::parse("${hole.k}-${hole.kind}-${k}").unwrap();
        assert_eq!(
            tpl.expand_map(&map).unwrap(),
            "${hole.kind}-article-${hole.kind}"
        );
    }

    #[test]
    fn preserves_plain_dollar_and_question_characters() {
        let mut map = BTreeMap::new();
        map.insert("x".to_string(), "ok".to_string());
        let tpl = Template::parse("really? <$> costs $5 ${hole.x}").unwrap();
        assert_eq!(tpl.expand_map(&map).unwrap(), "really? <$> costs $5 ok");
    }

    #[test]
    fn reports_unknown_captures_and_invalid_placeholders() {
        let tpl = Template::parse("hello ${hole.missing}").unwrap();
        let err = tpl.expand_map(&BTreeMap::new()).unwrap_err();
        assert!(err.to_string().contains("missing"), "{err}");

        for bad in [
            "${",
            "${}",
            "${hole.}",
            "${hole.a b}",
            "${hole.a.b}",
            "${vars.a}",
            "${hole.a | trim}",
        ] {
            assert!(Template::parse(bad).is_err(), "expected error for {bad}");
        }
    }
}
