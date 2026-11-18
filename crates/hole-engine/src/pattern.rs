//! Pattern intermediate representation (IR) for `hole-engine`.

/// A Tomet-independent pattern expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pattern {
    /// Exact literal string match.
    Literal(String),
    /// Matches any single Unicode scalar value.
    AnyChar {
        /// Whether `\n` is matched.
        cross_newline: bool,
    },
    /// Matches a single Unicode scalar value belonging to a character class.
    Class(CharClass),
    /// Matches a sequence of sub-patterns in order.
    Seq(Vec<Pattern>),
    /// Ordered choice: tries each branch in order and commits to the first match.
    Alt(Vec<Pattern>),
    /// Repetition of an inner pattern (`min..=max`).
    Repeat {
        inner: Box<Pattern>,
        min: usize,
        max: Option<usize>,
        greedy: bool,
    },
    /// Negative lookahead: succeeds (consuming zero bytes) iff `inner` fails.
    Not(Box<Pattern>),
    /// Positive lookahead: succeeds (consuming zero bytes) iff `inner` succeeds.
    Lookahead(Box<Pattern>),
    /// Matches the start of the input.
    Start,
    /// Matches the end of the input.
    End,
    /// Matches a word boundary between word (`[A-Za-z0-9_]`) and non-word positions.
    WordBoundary,
    /// Captures the matched slice (and nested child captures) under an optional name.
    Capture {
        name: Option<String>,
        inner: Box<Pattern>,
    },
    /// Comby-style balanced delimiter matcher (`open` ... `close`, supporting nesting).
    Balanced {
        open: String,
        close: String,
    },
    /// Built-in structured matcher (types, naming conventions, path segments).
    Builtin(BuiltinMatcher),
}

/// Character classes for single-character matching.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CharClass {
    Digit,
    Alpha,
    Alnum,
    Hex,
    Ascii,
    Space,
    NonSpace,
    Word,
    Range(char, char),
    OneOf(String),
    NoneOf(String),
}

impl CharClass {
    /// Returns `true` if `ch` belongs to this character class.
    pub fn matches(&self, ch: char) -> bool {
        match self {
            CharClass::Digit => ch.is_ascii_digit(),
            CharClass::Alpha => ch.is_alphabetic(),
            CharClass::Alnum => ch.is_alphanumeric(),
            CharClass::Hex => ch.is_ascii_hexdigit(),
            CharClass::Ascii => ch.is_ascii(),
            CharClass::Space => ch.is_whitespace(),
            CharClass::NonSpace => !ch.is_whitespace(),
            CharClass::Word => ch.is_alphanumeric() || ch == '_',
            CharClass::Range(start, end) => (*start..=*end).contains(&ch),
            CharClass::OneOf(chars) => chars.contains(ch),
            CharClass::NoneOf(chars) => !chars.contains(ch),
        }
    }
}

/// Case and naming convention styles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaseStyle {
    /// `kebab-case`: lowercase ASCII letters/digits separated by single `-`.
    Kebab,
    /// `snake_case`: lowercase ASCII letters/digits separated by single `_`.
    Snake,
    /// `camelCase`: starts with lowercase ASCII letter, followed by ASCII alphanumerics.
    Camel,
    /// `PascalCase`: starts with uppercase ASCII letter, followed by ASCII alphanumerics.
    Pascal,
    /// `SCREAMING_SNAKE_CASE`: uppercase ASCII letters/digits separated by single `_`.
    ScreamingSnake,
    /// URL/file slug: lowercase ASCII letters/digits separated by single `-` or `_`.
    Slug,
}

/// Built-in domain and type matchers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BuiltinMatcher {
    /// Signed integer (`-?[0-9]+`), optionally bounded by `min..=max`.
    Int {
        min: Option<i64>,
        max: Option<i64>,
    },
    /// Unsigned integer (`[0-9]+`), optionally bounded by `min..=max`.
    Uint {
        min: Option<u64>,
        max: Option<u64>,
    },
    /// Semantic version (`MAJOR.MINOR.PATCH` with optional `-pre` and `+build`).
    Version,
    /// ISO 8601 calendar date (`YYYY-MM-DD`).
    Date,
    /// Canonical hyphenated UUID (`8-4-4-4-12` hex digits).
    Uuid,
    /// Naming convention matcher.
    Case(CaseStyle),
    /// Non-empty single path segment (never crosses `/`).
    Seg,
}

impl Pattern {
    /// Creates a literal pattern.
    pub fn lit(s: impl Into<String>) -> Self {
        Self::Literal(s.into())
    }

    /// Creates a sequence pattern.
    pub fn seq(parts: Vec<Pattern>) -> Self {
        Self::Seq(parts)
    }

    /// Creates an ordered-choice alternation pattern.
    pub fn alt(branches: Vec<Pattern>) -> Self {
        Self::Alt(branches)
    }

    /// Creates an optional (`0..=1`) greedy pattern.
    pub fn opt(inner: Pattern) -> Self {
        Self::Repeat {
            inner: Box::new(inner),
            min: 0,
            max: Some(1),
            greedy: true,
        }
    }

    /// Creates a zero-or-more (`0..`) greedy pattern.
    pub fn many(inner: Pattern) -> Self {
        Self::Repeat {
            inner: Box::new(inner),
            min: 0,
            max: None,
            greedy: true,
        }
    }

    /// Creates a one-or-more (`1..`) greedy pattern.
    pub fn many1(inner: Pattern) -> Self {
        Self::Repeat {
            inner: Box::new(inner),
            min: 1,
            max: None,
            greedy: true,
        }
    }

    /// Wraps `self` in a named or anonymous capture.
    pub fn capture(self, name: Option<String>) -> Self {
        Self::Capture {
            name,
            inner: Box::new(self),
        }
    }
}
