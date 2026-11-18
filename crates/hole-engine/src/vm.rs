//! Bytecode virtual machine and built-in matcher evaluation for `hole-engine`.

use crate::pattern::{BuiltinMatcher, CaseStyle};
use crate::program::{Op, Program};

const DEFAULT_MAX_STEPS: usize = 1_000_000;

/// A hierarchical capture produced by a `Capture` node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capture {
    pub name: Option<String>,
    pub start: usize,
    pub end: usize,
    pub text: String,
    pub children: Vec<Capture>,
}

impl Capture {
    /// Finds the first descendant (or self) capture with the given `name`.
    pub fn get(&self, name: &str) -> Option<&Capture> {
        if self.name.as_deref() == Some(name) {
            return Some(self);
        }
        for child in &self.children {
            if let Some(found) = child.get(name) {
                return Some(found);
            }
        }
        None
    }
}

/// The result of a successful pattern match.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Match {
    pub start: usize,
    pub end: usize,
    pub text: String,
    pub captures: Vec<Capture>,
}

impl Match {
    /// Finds the first capture with the given `name` in the match tree.
    pub fn get(&self, name: &str) -> Option<&Capture> {
        for cap in &self.captures {
            if let Some(found) = cap.get(name) {
                return Some(found);
            }
        }
        None
    }
}

#[derive(Debug, Clone)]
enum CapEvent {
    Open { name: Option<String>, pos: usize },
    Close { pos: usize },
}

#[derive(Debug, Clone)]
enum Frame {
    Choice {
        pc: usize,
        pos: usize,
        cap_len: usize,
    },
    Not {
        after_pc: usize,
        pos: usize,
        cap_len: usize,
    },
    Lookahead {
        pos: usize,
        cap_len: usize,
    },
}

impl Program {
    /// Returns `true` if `input` matches the entire string from start to end.
    pub fn is_full_match(&self, input: &str) -> bool {
        self.match_full(input).is_some()
    }

    /// Matches `input` from start (`0`) to end (`input.len()`).
    pub fn match_full(&self, input: &str) -> Option<Match> {
        let m = self.match_at(input, 0)?;
        if m.end == input.len() { Some(m) } else { None }
    }

    /// Attempts to match `input` starting at byte offset `start_pos`.
    pub fn match_at(&self, input: &str, start_pos: usize) -> Option<Match> {
        if !input.is_char_boundary(start_pos) {
            return None;
        }
        let ops = &self.ops;
        let mut pc = 0usize;
        let mut pos = start_pos;
        let mut stack: Vec<Frame> = Vec::new();
        let mut caps: Vec<CapEvent> = Vec::new();
        let mut steps = 0usize;

        macro_rules! backtrack {
            () => {{
                let mut jumped = false;
                while let Some(frame) = stack.pop() {
                    match frame {
                        Frame::Choice {
                            pc: next_pc,
                            pos: saved_pos,
                            cap_len,
                        } => {
                            pc = next_pc;
                            pos = saved_pos;
                            caps.truncate(cap_len);
                            jumped = true;
                            break;
                        }
                        Frame::Not {
                            after_pc,
                            pos: saved_pos,
                            cap_len,
                        } => {
                            pc = after_pc;
                            pos = saved_pos;
                            caps.truncate(cap_len);
                            jumped = true;
                            break;
                        }
                        Frame::Lookahead { .. } => {
                            // Propagate failure past the lookahead boundary.
                        }
                    }
                }
                if !jumped {
                    return None;
                }
                continue;
            }};
        }

        loop {
            steps += 1;
            if steps > DEFAULT_MAX_STEPS {
                return None;
            }
            let op = ops.get(pc)?;
            match op {
                Op::Match => {
                    let captures = build_captures(input, &caps);
                    return Some(Match {
                        start: start_pos,
                        end: pos,
                        text: input[start_pos..pos].to_string(),
                        captures,
                    });
                }
                Op::Literal(lit) => {
                    if input[pos..].starts_with(lit.as_str()) {
                        pos += lit.len();
                        pc += 1;
                    } else {
                        backtrack!();
                    }
                }
                Op::AnyChar { cross_newline } => {
                    if let Some(ch) = input[pos..].chars().next() {
                        if *cross_newline || ch != '\n' {
                            pos += ch.len_utf8();
                            pc += 1;
                            continue;
                        }
                    }
                    backtrack!();
                }
                Op::Class(class) => {
                    if let Some(ch) = input[pos..].chars().next() {
                        if class.matches(ch) {
                            pos += ch.len_utf8();
                            pc += 1;
                            continue;
                        }
                    }
                    backtrack!();
                }
                Op::Builtin(builtin) => {
                    if let Some(consumed) = match_builtin(builtin, &input[pos..]) {
                        pos += consumed;
                        pc += 1;
                    } else {
                        backtrack!();
                    }
                }
                Op::Balanced { open, close } => {
                    if let Some(consumed) = match_balanced(&input[pos..], open, close) {
                        pos += consumed;
                        pc += 1;
                    } else {
                        backtrack!();
                    }
                }
                Op::StartAnchor => {
                    if pos == 0 {
                        pc += 1;
                    } else {
                        backtrack!();
                    }
                }
                Op::EndAnchor => {
                    if pos == input.len() {
                        pc += 1;
                    } else {
                        backtrack!();
                    }
                }
                Op::WordBoundary => {
                    if is_word_boundary(input, pos) {
                        pc += 1;
                    } else {
                        backtrack!();
                    }
                }
                Op::Jump(target) => {
                    pc = *target;
                }
                Op::Choice(alt_pc) => {
                    stack.push(Frame::Choice {
                        pc: *alt_pc,
                        pos,
                        cap_len: caps.len(),
                    });
                    pc += 1;
                }
                Op::Commit(target) => {
                    // Pop the most recent Choice frame and jump.
                    while let Some(frame) = stack.pop() {
                        if matches!(frame, Frame::Choice { .. }) {
                            break;
                        }
                    }
                    pc = *target;
                }
                Op::DropChoice => {
                    while let Some(frame) = stack.pop() {
                        if matches!(frame, Frame::Choice { .. }) {
                            break;
                        }
                    }
                    pc += 1;
                }
                Op::OpenCapture(name) => {
                    caps.push(CapEvent::Open {
                        name: name.clone(),
                        pos,
                    });
                    pc += 1;
                }
                Op::CloseCapture => {
                    caps.push(CapEvent::Close { pos });
                    pc += 1;
                }
                Op::NotStart(after_pc) => {
                    stack.push(Frame::Not {
                        after_pc: *after_pc,
                        pos,
                        cap_len: caps.len(),
                    });
                    pc += 1;
                }
                Op::NotFail => {
                    // Inner succeeded: discard up to Not frame, then fail outer.
                    while let Some(frame) = stack.pop() {
                        if matches!(frame, Frame::Not { .. }) {
                            break;
                        }
                    }
                    backtrack!();
                }
                Op::LookaheadStart(_) => {
                    stack.push(Frame::Lookahead {
                        pos,
                        cap_len: caps.len(),
                    });
                    pc += 1;
                }
                Op::LookaheadEnd => {
                    while let Some(frame) = stack.pop() {
                        if let Frame::Lookahead {
                            pos: saved_pos,
                            cap_len,
                        } = frame
                        {
                            pos = saved_pos;
                            caps.truncate(cap_len);
                            break;
                        }
                    }
                    pc += 1;
                }
            }
        }
    }

    /// Finds the first match anywhere in `input`, scanning left to right.
    pub fn find(&self, input: &str) -> Option<Match> {
        for (idx, _) in input.char_indices() {
            if let Some(m) = self.match_at(input, idx) {
                return Some(m);
            }
        }
        // Also try empty match at input.len()
        self.match_at(input, input.len())
    }

    /// Finds all non-overlapping matches in `input`.
    pub fn find_all(&self, input: &str) -> Vec<Match> {
        let mut out = Vec::new();
        let mut pos = 0usize;
        while pos <= input.len() {
            if let Some(m) = self.match_at(input, pos) {
                let next = if m.end > pos {
                    m.end
                } else if pos < input.len() {
                    let ch_len = input[pos..].chars().next().map_or(1, char::len_utf8);
                    pos + ch_len
                } else {
                    input.len() + 1
                };
                out.push(m);
                pos = next;
            } else if pos < input.len() {
                let ch_len = input[pos..].chars().next().map_or(1, char::len_utf8);
                pos += ch_len;
            } else {
                break;
            }
        }
        out
    }
}

fn build_captures(input: &str, events: &[CapEvent]) -> Vec<Capture> {
    struct OpenCap {
        name: Option<String>,
        start: usize,
        children: Vec<Capture>,
    }

    let mut roots = Vec::new();
    let mut stack: Vec<OpenCap> = Vec::new();

    for ev in events {
        match ev {
            CapEvent::Open { name, pos } => stack.push(OpenCap {
                name: name.clone(),
                start: *pos,
                children: Vec::new(),
            }),
            CapEvent::Close { pos } => {
                if let Some(open) = stack.pop() {
                    let cap = Capture {
                        name: open.name,
                        start: open.start,
                        end: *pos,
                        text: input[open.start..*pos].to_string(),
                        children: open.children,
                    };
                    if let Some(parent) = stack.last_mut() {
                        parent.children.push(cap);
                    } else {
                        roots.push(cap);
                    }
                }
            }
        }
    }

    roots
}

fn is_word_char(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_'
}

fn is_word_boundary(input: &str, pos: usize) -> bool {
    let before = input[..pos].chars().next_back().is_some_and(is_word_char);
    let after = input[pos..].chars().next().is_some_and(is_word_char);
    before != after
}

fn match_balanced(s: &str, open: &str, close: &str) -> Option<usize> {
    if open.is_empty() || close.is_empty() || !s.starts_with(open) {
        return None;
    }
    let mut depth = 0usize;
    let mut i = 0usize;
    while i < s.len() {
        if s[i..].starts_with(open) {
            depth += 1;
            i += open.len();
        } else if s[i..].starts_with(close) {
            depth = depth.checked_sub(1)?;
            i += close.len();
            if depth == 0 {
                return Some(i);
            }
        } else {
            let ch = s[i..].chars().next()?;
            i += ch.len_utf8();
        }
    }
    None
}

fn match_builtin(builtin: &BuiltinMatcher, s: &str) -> Option<usize> {
    match builtin {
        BuiltinMatcher::Int { min, max } => match_int(s, *min, *max),
        BuiltinMatcher::Uint { min, max } => match_uint(s, *min, *max),
        BuiltinMatcher::Version => match_version(s),
        BuiltinMatcher::Date => match_date(s),
        BuiltinMatcher::Uuid => match_uuid(s),
        BuiltinMatcher::Case(style) => match_case(s, *style),
        BuiltinMatcher::Seg => match_seg(s),
    }
}

fn match_int(s: &str, min: Option<i64>, max: Option<i64>) -> Option<usize> {
    let mut len = 0usize;
    let bytes = s.as_bytes();
    if bytes.first() == Some(&b'-') {
        len = 1;
    }
    let digits_start = len;
    while len < bytes.len() && bytes[len].is_ascii_digit() {
        len += 1;
    }
    if len == digits_start {
        return None;
    }
    let val: i64 = s[..len].parse().ok()?;
    if let Some(m) = min {
        if val < m {
            return None;
        }
    }
    if let Some(m) = max {
        if val > m {
            return None;
        }
    }
    Some(len)
}

fn match_uint(s: &str, min: Option<u64>, max: Option<u64>) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut len = 0usize;
    while len < bytes.len() && bytes[len].is_ascii_digit() {
        len += 1;
    }
    if len == 0 {
        return None;
    }
    let val: u64 = s[..len].parse().ok()?;
    if let Some(m) = min {
        if val < m {
            return None;
        }
    }
    if let Some(m) = max {
        if val > m {
            return None;
        }
    }
    Some(len)
}

fn scan_digits(s: &str) -> usize {
    s.bytes().take_while(|b| b.is_ascii_digit()).count()
}

fn match_version(s: &str) -> Option<usize> {
    let mut i = 0usize;
    let major = scan_digits(&s[i..]);
    if major == 0 {
        return None;
    }
    i += major;
    if !s[i..].starts_with('.') {
        return None;
    }
    i += 1;
    let minor = scan_digits(&s[i..]);
    if minor == 0 {
        return None;
    }
    i += minor;
    if !s[i..].starts_with('.') {
        return None;
    }
    i += 1;
    let patch = scan_digits(&s[i..]);
    if patch == 0 {
        return None;
    }
    i += patch;

    if s[i..].starts_with('-') {
        let rest = &s[i + 1..];
        let pre_len = rest
            .bytes()
            .take_while(|b| b.is_ascii_alphanumeric() || *b == b'-' || *b == b'.')
            .count();
        if pre_len > 0 {
            i += 1 + pre_len;
        }
    }
    if s[i..].starts_with('+') {
        let rest = &s[i + 1..];
        let build_len = rest
            .bytes()
            .take_while(|b| b.is_ascii_alphanumeric() || *b == b'-' || *b == b'.')
            .count();
        if build_len > 0 {
            i += 1 + build_len;
        }
    }
    Some(i)
}

fn match_date(s: &str) -> Option<usize> {
    let b = s.as_bytes();
    if b.len() < 10 {
        return None;
    }
    let is_digits = |slice: &[u8]| slice.iter().all(|c| c.is_ascii_digit());
    if is_digits(&b[0..4])
        && b[4] == b'-'
        && is_digits(&b[5..7])
        && b[7] == b'-'
        && is_digits(&b[8..10])
    {
        let month: u8 = s[5..7].parse().ok()?;
        let day: u8 = s[8..10].parse().ok()?;
        if (1..=12).contains(&month) && (1..=31).contains(&day) {
            return Some(10);
        }
    }
    None
}

fn match_uuid(s: &str) -> Option<usize> {
    let b = s.as_bytes();
    if b.len() < 36 {
        return None;
    }
    let is_hex = |slice: &[u8]| slice.iter().all(|c| c.is_ascii_hexdigit());
    if is_hex(&b[0..8])
        && b[8] == b'-'
        && is_hex(&b[9..13])
        && b[13] == b'-'
        && is_hex(&b[14..18])
        && b[18] == b'-'
        && is_hex(&b[19..23])
        && b[23] == b'-'
        && is_hex(&b[24..36])
    {
        Some(36)
    } else {
        None
    }
}

fn match_separated_case(s: &str, sep: u8, allow_alt_sep: Option<u8>, uppercase: bool) -> Option<usize> {
    let b = s.as_bytes();
    let is_atom = |c: u8| {
        if uppercase {
            c.is_ascii_uppercase() || c.is_ascii_digit()
        } else {
            c.is_ascii_lowercase() || c.is_ascii_digit()
        }
    };
    let is_first = |c: u8| {
        if uppercase {
            c.is_ascii_uppercase()
        } else {
            c.is_ascii_lowercase()
        }
    };
    if b.is_empty() || !is_first(b[0]) {
        return None;
    }
    let mut i = 1usize;
    while i < b.len() {
        if is_atom(b[i]) {
            i += 1;
        } else if (b[i] == sep || allow_alt_sep.is_some_and(|a| b[i] == a))
            && i + 1 < b.len()
            && is_atom(b[i + 1])
        {
            i += 2;
        } else {
            break;
        }
    }
    Some(i)
}

fn match_case(s: &str, style: CaseStyle) -> Option<usize> {
    match style {
        CaseStyle::Kebab => match_separated_case(s, b'-', None, false),
        CaseStyle::Snake => match_separated_case(s, b'_', None, false),
        CaseStyle::ScreamingSnake => match_separated_case(s, b'_', None, true),
        CaseStyle::Slug => match_separated_case(s, b'-', Some(b'_'), false),
        CaseStyle::Camel => {
            let b = s.as_bytes();
            if b.is_empty() || !b[0].is_ascii_lowercase() {
                return None;
            }
            let mut i = 1usize;
            while i < b.len() && b[i].is_ascii_alphanumeric() {
                i += 1;
            }
            Some(i)
        }
        CaseStyle::Pascal => {
            let b = s.as_bytes();
            if b.is_empty() || !b[0].is_ascii_uppercase() {
                return None;
            }
            let mut i = 1usize;
            while i < b.len() && b[i].is_ascii_alphanumeric() {
                i += 1;
            }
            Some(i)
        }
    }
}

fn match_seg(s: &str) -> Option<usize> {
    let len = s.bytes().take_while(|&b| b != b'/').count();
    if len == 0 { None } else { Some(len) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pattern::{BuiltinMatcher, CaseStyle, CharClass, Pattern};

    #[test]
    fn nested_captures_and_builtins() {
        // Equivalent to: <?(id)>[adr-<?int(n)>-<?kebab(slug)>].md
        let pat = Pattern::seq(vec![
            Pattern::seq(vec![
                Pattern::lit("adr-"),
                Pattern::Builtin(BuiltinMatcher::Int {
                    min: Some(1),
                    max: None,
                })
                .capture(Some("n".into())),
                Pattern::lit("-"),
                Pattern::Builtin(BuiltinMatcher::Case(CaseStyle::Kebab))
                    .capture(Some("slug".into())),
            ])
            .capture(Some("id".into())),
            Pattern::lit(".md"),
        ]);

        let prog = Program::compile(&pat);
        let m = prog.match_full("adr-0042-zone-design.md").unwrap();
        assert_eq!(m.get("id").unwrap().text, "adr-0042-zone-design");
        assert_eq!(m.get("n").unwrap().text, "0042");
        assert_eq!(m.get("slug").unwrap().text, "zone-design");

        assert!(prog.match_full("adr-0-bad_name.md").is_none());
        assert!(prog.match_full("adr-0001-bad_name.md").is_none());
    }

    #[test]
    fn greedy_repeat_backtracks_for_suffix() {
        let pat = Pattern::seq(vec![
            Pattern::many1(Pattern::Class(CharClass::NonSpace)).capture(Some("stem".into())),
            Pattern::lit(".tmt"),
        ]);
        let prog = Program::compile(&pat);
        let m = prog.match_full("readme.tmt").unwrap();
        assert_eq!(m.get("stem").unwrap().text, "readme");
    }

    #[test]
    fn balanced_delimiters_nested() {
        let pat = Pattern::seq(vec![
            Pattern::lit("fn "),
            Pattern::Builtin(BuiltinMatcher::Case(CaseStyle::Snake)).capture(Some("name".into())),
            Pattern::Balanced {
                open: "(".into(),
                close: ")".into(),
            }
            .capture(Some("params".into())),
        ]);
        let prog = Program::compile(&pat);
        let m = prog.match_full("fn foo_bar(a: (i32, i32), b: u8)").unwrap();
        assert_eq!(m.get("name").unwrap().text, "foo_bar");
        assert_eq!(m.get("params").unwrap().text, "(a: (i32, i32), b: u8)");
    }

    #[test]
    fn negative_lookahead_and_version() {
        let pat = Pattern::seq(vec![
            Pattern::Not(Box::new(Pattern::lit("deprecated-"))),
            Pattern::Builtin(BuiltinMatcher::Case(CaseStyle::Kebab)).capture(Some("crate".into())),
            Pattern::lit("@"),
            Pattern::Builtin(BuiltinMatcher::Version).capture(Some("ver".into())),
            Pattern::lit(".vocabulary.tmt"),
        ]);
        let prog = Program::compile(&pat);
        let m = prog.match_full("catasto@1.0.0.vocabulary.tmt").unwrap();
        assert_eq!(m.get("crate").unwrap().text, "catasto");
        assert_eq!(m.get("ver").unwrap().text, "1.0.0");

        assert!(prog.match_full("deprecated-old@1.0.0.vocabulary.tmt").is_none());
    }

    #[test]
    fn find_all_words() {
        let pat = Pattern::seq(vec![
            Pattern::WordBoundary,
            Pattern::Builtin(BuiltinMatcher::Case(CaseStyle::Kebab)).capture(Some("k".into())),
            Pattern::WordBoundary,
        ]);
        let prog = Program::compile(&pat);
        let matches = prog.find_all("use foo-bar and baz-qux!");
        let found: Vec<&str> = matches.iter().map(|m| m.text.as_str()).collect();
        assert_eq!(found, vec!["use", "foo-bar", "and", "baz-qux"]);
    }
}
