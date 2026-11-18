//! Bytecode virtual machine and built-in matcher evaluation for `hole-engine`.

use crate::pattern::{BuiltinMatcher, CaseStyle, DEFAULT_NANOID_LENGTH};
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

    /// Collects all captures matching `name` in pre-order (self and descendants).
    pub fn get_all<'a>(&'a self, name: &str, out: &mut Vec<&'a Capture>) {
        if self.name.as_deref() == Some(name) {
            out.push(self);
        }
        for child in &self.children {
            child.get_all(name, out);
        }
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

    /// Returns all captures with the given `name` in pre-order.
    pub fn get_all(&self, name: &str) -> Vec<&Capture> {
        let mut out = Vec::new();
        for cap in &self.captures {
            cap.get_all(name, &mut out);
        }
        out
    }

    /// Expands a replacement [`Template`](crate::Template) against this match's captures.
    pub fn expand(&self, template: &crate::Template) -> Result<String, crate::TemplateError> {
        template.expand(self)
    }
}

#[derive(Debug, Clone)]
enum CapEvent {
    Open {
        name: Option<String>,
        site: usize,
        pos: usize,
    },
    Close {
        name: Option<String>,
        site: usize,
        start: usize,
        end: usize,
    },
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
        self.run(input, 0, true)
    }

    /// Attempts to match `input` starting at byte offset `start_pos`.
    pub fn match_at(&self, input: &str, start_pos: usize) -> Option<Match> {
        self.run(input, start_pos, false)
    }

    fn run(&self, input: &str, start_pos: usize, require_end: bool) -> Option<Match> {
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
                    if require_end && pos != input.len() {
                        backtrack!();
                    }
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
                Op::OpenCapture { name, site } => {
                    caps.push(CapEvent::Open {
                        name: name.clone(),
                        site: *site,
                        pos,
                    });
                    pc += 1;
                }
                Op::CloseCapture => {
                    let (open_name, site, start) = find_matching_open(&caps)?;
                    if let Some(ref name) = open_name {
                        if let Some((prev_start, prev_end)) =
                            find_first_closed_other_site(&caps, name, site)
                        {
                            if input[prev_start..prev_end] != input[start..pos] {
                                backtrack!();
                            }
                        }
                    }
                    caps.push(CapEvent::Close {
                        name: open_name,
                        site,
                        start,
                        end: pos,
                    });
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

fn find_matching_open(caps: &[CapEvent]) -> Option<(Option<String>, usize, usize)> {
    let mut depth = 0usize;
    for ev in caps.iter().rev() {
        match ev {
            CapEvent::Close { .. } => depth += 1,
            CapEvent::Open { name, site, pos } => {
                if depth == 0 {
                    return Some((name.clone(), *site, *pos));
                }
                depth -= 1;
            }
        }
    }
    None
}

fn find_first_closed_other_site(
    caps: &[CapEvent],
    target: &str,
    current_site: usize,
) -> Option<(usize, usize)> {
    for ev in caps {
        if let CapEvent::Close {
            name: Some(name),
            site,
            start,
            end,
        } = ev
        {
            if name == target && *site != current_site {
                return Some((*start, *end));
            }
        }
    }
    None
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
            CapEvent::Open { name, pos, .. } => stack.push(OpenCap {
                name: name.clone(),
                start: *pos,
                children: Vec::new(),
            }),
            CapEvent::Close { end, .. } => {
                if let Some(open) = stack.pop() {
                    let cap = Capture {
                        name: open.name,
                        start: open.start,
                        end: *end,
                        text: input[open.start..*end].to_string(),
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
        BuiltinMatcher::Float => match_float(s),
        BuiltinMatcher::Bool => match_bool(s),
        BuiltinMatcher::Version => match_version(s),
        BuiltinMatcher::Date => match_date(s),
        BuiltinMatcher::Datetime => match_datetime(s),
        BuiltinMatcher::Uuid => match_uuid(s),
        BuiltinMatcher::Nanoid { length } => match_nanoid(s, *length),
        BuiltinMatcher::Case(style) => match_case(s, *style),
        BuiltinMatcher::Seg => match_seg(s),
        BuiltinMatcher::Tree { .. } => None,
    }
}

fn match_float(s: &str) -> Option<usize> {
    let b = s.as_bytes();
    let mut i = usize::from(b.first() == Some(&b'-'));
    let int_digits = scan_digits(&s[i..]);
    if int_digits == 0 {
        return None;
    }
    i += int_digits;

    if b.get(i) == Some(&b'.') {
        let frac_digits = scan_digits(&s[i + 1..]);
        if frac_digits > 0 {
            i += 1 + frac_digits;
        }
    }

    if matches!(b.get(i), Some(b'e' | b'E')) {
        let mut exp_i = i + 1;
        if matches!(b.get(exp_i), Some(b'+' | b'-')) {
            exp_i += 1;
        }
        let exp_digits = scan_digits(&s[exp_i..]);
        if exp_digits > 0 {
            i = exp_i + exp_digits;
        }
    }

    Some(i)
}

fn match_bool(s: &str) -> Option<usize> {
    if s.starts_with("true") {
        Some(4)
    } else if s.starts_with("false") {
        Some(5)
    } else {
        None
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

fn two_digits_at(b: &[u8]) -> Option<u8> {
    if b.len() >= 2 && b[0].is_ascii_digit() && b[1].is_ascii_digit() {
        Some((b[0] - b'0') * 10 + (b[1] - b'0'))
    } else {
        None
    }
}

fn days_in_month(year: u16, month: u8) -> u8 {
    match month {
        2 if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

fn parse_date_prefix(s: &str) -> Option<usize> {
    let b = s.as_bytes();
    if b.len() < 10 {
        return None;
    }
    if !b[0..4].iter().all(|c| c.is_ascii_digit()) || b[4] != b'-' || b[7] != b'-' {
        return None;
    }
    let year: u16 = s[0..4].parse().ok()?;
    let month = two_digits_at(&b[5..7])?;
    let day = two_digits_at(&b[8..10])?;
    if !(1..=12).contains(&month) || day == 0 || day > days_in_month(year, month) {
        return None;
    }
    Some(10)
}

fn parse_clock_prefix(s: &str) -> Option<usize> {
    let b = s.as_bytes();
    if b.len() < 5 || b[2] != b':' {
        return None;
    }
    let hours = two_digits_at(&b[0..2])?;
    let minutes = two_digits_at(&b[3..5])?;
    if hours > 23 || minutes > 59 {
        return None;
    }
    let mut len = 5usize;
    if b.get(len) == Some(&b':') {
        let sec = two_digits_at(b.get(len + 1..len + 3)?)?;
        if sec > 59 {
            return None;
        }
        len += 3;
        if b.get(len) == Some(&b'.') {
            let frac_digits = scan_digits(&s[len + 1..]);
            if (1..=9).contains(&frac_digits) {
                len += 1 + frac_digits;
            }
        }
    }
    Some(len)
}

fn parse_offset_prefix(s: &str) -> Option<usize> {
    let b = s.as_bytes();
    if b.first() == Some(&b'Z') {
        return Some(1);
    }
    if b.len() < 6 || !matches!(b[0], b'+' | b'-') || b[3] != b':' {
        return None;
    }
    let hours = two_digits_at(&b[1..3])?;
    let minutes = two_digits_at(&b[4..6])?;
    if hours > 18 || minutes >= 60 {
        return None;
    }
    Some(6)
}

fn match_date(s: &str) -> Option<usize> {
    parse_date_prefix(s)
}

fn match_datetime(s: &str) -> Option<usize> {
    let date_len = parse_date_prefix(s)?;
    let rest = &s[date_len..];
    if let Some(after_sep) = rest.strip_prefix('T').or_else(|| rest.strip_prefix(' ')) {
        if let Some(clock_len) = parse_clock_prefix(after_sep) {
            let mut len = date_len + 1 + clock_len;
            if let Some(off_len) = parse_offset_prefix(&s[len..]) {
                len += off_len;
            }
            return Some(len);
        }
    }
    Some(date_len)
}

fn match_nanoid(s: &str, length: Option<usize>) -> Option<usize> {
    let target = length.unwrap_or(DEFAULT_NANOID_LENGTH);
    if target == 0 || s.len() < target {
        return None;
    }
    let is_nanoid_byte = |b: u8| b.is_ascii_alphanumeric() || b == b'_' || b == b'-';
    if s.as_bytes()[..target].iter().all(|&b| is_nanoid_byte(b)) {
        Some(target)
    } else {
        None
    }
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

    #[test]
    fn path_seg_and_tree_matching_and_backtracking() {
        let seg_suffix = Program::compile(&Pattern::seq(vec![
            Pattern::seg().capture(Some("stem".into())),
            Pattern::lit(".rs"),
        ]));
        assert_eq!(
            seg_suffix.match_full("lib.rs").unwrap().get("stem").unwrap().text,
            "lib"
        );
        assert!(seg_suffix.match_full("src/lib.rs").is_none());
        assert!(seg_suffix.match_full(".rs").is_none());

        // Tree with suffix and directory boundary backtracking
        let tree_dir = Program::compile(&Pattern::seq(vec![
            Pattern::lit("src/"),
            Pattern::tree(1, None).capture(Some("sub".into())),
            Pattern::lit("/mod.rs"),
        ]));
        assert_eq!(
            tree_dir
                .match_full("src/a/b/mod.rs")
                .unwrap()
                .get("sub")
                .unwrap()
                .text,
            "a/b"
        );
        assert!(tree_dir.match_full("src/mod.rs").is_none());
        assert!(tree_dir.match_full("src//mod.rs").is_none());

        let tree_ext = Program::compile(&Pattern::seq(vec![
            Pattern::tree(1, Some(3)).capture(Some("path".into())),
            Pattern::lit(".tmt"),
        ]));
        assert_eq!(
            tree_ext
                .match_full("a/b/c.tmt")
                .unwrap()
                .get("path")
                .unwrap()
                .text,
            "a/b/c"
        );
        assert!(tree_ext.match_full("a/b/c/d.tmt").is_none());
        assert!(tree_ext.match_full("/a/b.tmt").is_none());
        assert!(tree_ext.match_full("a//b.tmt").is_none());
        assert!(tree_ext.match_full("a/b/.tmt").is_none());

        // min_depth: 0 allows empty match
        let tree_opt = Program::compile(&Pattern::tree(0, Some(2)));
        assert!(tree_opt.is_full_match(""));
        assert!(tree_opt.is_full_match("a"));
        assert!(tree_opt.is_full_match("a/b"));
        assert!(!tree_opt.is_full_match("a/b/c"));
    }

    #[test]
    fn float_bool_datetime_date_and_nanoid_builtins() {
        let float_prog = Program::compile(&Pattern::Builtin(BuiltinMatcher::Float));
        for ok in ["0", "42", "-7", "3.14", "-0.25", "1e10", "2.5E-3", "-1.0e+2"] {
            assert!(float_prog.is_full_match(ok), "should match float: {ok}");
        }
        for bad in ["", "-", ".5", "3.", "1e", "1e+", "abc"] {
            assert!(!float_prog.is_full_match(bad), "should reject float: {bad}");
        }

        let bool_prog = Program::compile(&Pattern::Builtin(BuiltinMatcher::Bool));
        assert!(bool_prog.is_full_match("true"));
        assert!(bool_prog.is_full_match("false"));
        assert!(!bool_prog.is_full_match("trueish"));
        assert!(!bool_prog.is_full_match("TRUE"));

        let date_prog = Program::compile(&Pattern::Builtin(BuiltinMatcher::Date));
        assert!(date_prog.is_full_match("2024-02-29"));
        assert!(!date_prog.is_full_match("2026-02-29"));
        assert!(!date_prog.is_full_match("2026-04-31"));
        assert!(!date_prog.is_full_match("2026-10-25T09:00"));

        let dt_prog = Program::compile(&Pattern::Builtin(BuiltinMatcher::Datetime));
        for ok in [
            "2026-10-25",
            "2024-02-29T09:00",
            "2026-10-25 09:00:00",
            "2026-10-25T09:00:00Z",
            "2026-10-25T09:00:00.5+09:00",
            "2026-10-25 23:59:59.123456789-05:30",
        ] {
            assert!(dt_prog.is_full_match(ok), "should match datetime: {ok}");
        }
        for bad in [
            "2026-02-29",
            "2026-10-25T24:00",
            "2026-10-25T09:60",
            "2026-10-25T09:00:60",
            "2026-10-25T09:00:00+19:00",
            "2026-10-25T09:00:00.1234567890",
        ] {
            assert!(!dt_prog.is_full_match(bad), "should reject datetime: {bad}");
        }

        // Datetime must not swallow surrounding text separators
        let dt_ctx = Program::compile(&Pattern::seq(vec![
            Pattern::Builtin(BuiltinMatcher::Datetime).capture(Some("dt".into())),
            Pattern::lit(" hello"),
        ]));
        assert_eq!(
            dt_ctx
                .match_full("2026-10-25 hello")
                .unwrap()
                .get("dt")
                .unwrap()
                .text,
            "2026-10-25"
        );

        let nano_default = Program::compile(&Pattern::Builtin(BuiltinMatcher::Nanoid { length: None }));
        assert!(nano_default.is_full_match("V1StGXR8_Z5jdHi6B-myT"));
        assert!(!nano_default.is_full_match("V1StGXR8_Z5jdHi6B-my"));
        assert!(!nano_default.is_full_match("V1StGXR8_Z5jdHi6B-myTX"));

        let nano_8 = Program::compile(&Pattern::Builtin(BuiltinMatcher::Nanoid { length: Some(8) }));
        assert!(nano_8.is_full_match("aB3_x-9Z"));
        assert!(!nano_8.is_full_match("aB3_x-9!"));
    }

    #[test]
    fn char_classes_sep_by_and_lookahead() {
        let cls = Program::compile(&Pattern::seq(vec![
            Pattern::range('a', 'z'),
            Pattern::chars("_-"),
            Pattern::not_chars("0123456789"),
        ]));
        assert!(cls.is_full_match("a-z"));
        assert!(!cls.is_full_match("A-z"));
        assert!(!cls.is_full_match("a-5"));

        let list_prog = Program::compile(&Pattern::sep_by(
            Pattern::Builtin(BuiltinMatcher::Int {
                min: None,
                max: None,
            })
            .capture(Some("n".into())),
            Pattern::lit(", "),
        ));
        assert!(list_prog.is_full_match(""));
        let m = list_prog.match_full("10, 20, -30").unwrap();
        let nums: Vec<&str> = m.get_all("n").into_iter().map(|c| c.text.as_str()).collect();
        assert_eq!(nums, vec!["10", "20", "-30"]);
        assert!(!list_prog.is_full_match("10, 20, "));

        let list1 = Program::compile(&Pattern::sep_by1(
            Pattern::Builtin(BuiltinMatcher::Case(CaseStyle::Kebab)),
            Pattern::lit("/"),
        ));
        assert!(!list1.is_full_match(""));
        assert!(list1.is_full_match("one/two-three"));

        // Positive + negative lookahead
        let la = Program::compile(&Pattern::seq(vec![
            Pattern::followed_by(Pattern::lit("foo")),
            Pattern::not_followed_by(Pattern::lit("foobar")),
            Pattern::many1(Pattern::Class(CharClass::Alpha)).capture(Some("word".into())),
        ]));
        assert_eq!(la.match_full("foobaz").unwrap().get("word").unwrap().text, "foobaz");
        assert!(la.match_full("foobar").is_none());
        assert!(la.match_full("bar").is_none());
    }

    #[test]
    fn repeated_binding_equality_and_greedy_backtracking() {
        let any_word = Pattern::many1(Pattern::AnyChar {
            cross_newline: false,
        });
        let pat = Pattern::seq(vec![
            any_word.clone().capture(Some("x".into())),
            Pattern::lit("-"),
            any_word.clone().capture(Some("x".into())),
        ]);
        let prog = Program::compile(&pat);

        // Simple equal vs unequal
        assert_eq!(prog.match_full("foo-foo").unwrap().get("x").unwrap().text, "foo");
        assert!(prog.match_full("foo-bar").is_none());

        // Greedy shrinking across internal hyphens:
        // First `x` initially grabs "a-a-a-a", shrinks until both sides are "a-a".
        let m = prog.match_full("a-a-a-a").unwrap();
        assert_eq!(m.get("x").unwrap().text, "a-a");
        assert_eq!(m.get_all("x").len(), 2);

        // Greedy prefix before middle wildcard must shrink for repeated binding to match
        let bridged = Program::compile(&Pattern::seq(vec![
            any_word.clone().capture(Some("x".into())),
            Pattern::lit(":"),
            any_word.clone().capture(Some("mid".into())),
            Pattern::lit(":"),
            any_word.clone().capture(Some("x".into())),
        ]));
        let mb = bridged.match_full("ab:cd:ef:ab").unwrap();
        assert_eq!(mb.get("x").unwrap().text, "ab");
        assert_eq!(mb.get("mid").unwrap().text, "cd:ef");

        // Three occurrences of `x`
        let triple = Program::compile(&Pattern::seq(vec![
            any_word.clone().capture(Some("x".into())),
            Pattern::lit("-"),
            any_word.clone().capture(Some("x".into())),
            Pattern::lit("-"),
            any_word.clone().capture(Some("x".into())),
        ]));
        assert_eq!(
            triple.match_full("ha-ha-ha").unwrap().get("x").unwrap().text,
            "ha"
        );
        assert!(triple.match_full("ha-ho-ha").is_none());
        assert!(triple.match_full("ha-ha-ho").is_none());

        // Cross-site equality with sep_by: first `x` binds, all `x` inside sep_by must match it
        let cross_sep = Program::compile(&Pattern::seq(vec![
            any_word.clone().capture(Some("x".into())),
            Pattern::lit(":"),
            Pattern::sep_by1(any_word.clone().capture(Some("x".into())), Pattern::lit(",")),
        ]));
        assert!(cross_sep.is_full_match("ab:ab,ab"));
        assert!(!cross_sep.is_full_match("ab:ab,cd"));

        // Negative lookahead referencing an earlier binding: `<?(x)>-<?not_followed_by>[<?(x)>]<?>`
        let not_same = Program::compile(&Pattern::seq(vec![
            any_word.clone().capture(Some("x".into())),
            Pattern::lit("-"),
            Pattern::not_followed_by(any_word.clone().capture(Some("x".into()))),
            any_word.capture(Some("y".into())),
        ]));
        assert_eq!(
            not_same.match_full("ab-cd").unwrap().get("y").unwrap().text,
            "cd"
        );
        assert!(not_same.match_full("ab-ab").is_none());
    }

    #[test]
    fn lazy_repeat_backtracks_in_full_match() {
        let lazy_pat = Pattern::seq(vec![
            Pattern::repeat(
                Pattern::AnyChar {
                    cross_newline: false,
                },
                1,
                None,
                false,
            )
            .capture(Some("x".into())),
            Pattern::lit("-"),
            Pattern::repeat(
                Pattern::AnyChar {
                    cross_newline: false,
                },
                1,
                None,
                false,
            )
            .capture(Some("x".into())),
        ]);
        let prog = Program::compile(&lazy_pat);
        // Lazy `x` tries "a" first (can't reach full end), backtracks to "a-a"
        let m = prog.match_full("a-a-a-a").unwrap();
        assert_eq!(m.get("x").unwrap().text, "a-a");
    }
}
