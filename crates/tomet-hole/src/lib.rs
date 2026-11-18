//! Bridge between `tomet-ast` `Hole` (`<?...>`) nodes and `hole-engine`.

use std::fmt;

use hole_engine::{BuiltinMatcher, CaseStyle, CharClass, Pattern};
use tomet_ast::{Block, Group, Hole, Inline, Tuple, Value};
use tomet_scalar::Scalar;

pub use hole_engine;

/// Compilation mode controlling how context-sensitive holes behave.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CompileMode {
    /// General text pattern matching: `<?>` matches one or more non-newline characters.
    #[default]
    Text,
    /// Filesystem path pattern matching: `<?>` defaults to a single non-empty segment
    /// without crossing `/`, while `<?tree>` crosses `/`.
    Path,
}

/// Error returned when a `tomet-ast` hole or block pattern cannot be compiled into a [`Pattern`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompileError {
    pub message: String,
}

impl CompileError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for CompileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for CompileError {}

/// Parses and compiles a Tomet pattern string in [`CompileMode::Text`] mode.
pub fn compile_str(src: &str) -> Result<Pattern, CompileError> {
    compile_str_with_mode(src, CompileMode::Text)
}

/// Parses and compiles a Tomet path pattern string in [`CompileMode::Path`] mode.
///
/// In path mode, `<?>` and `<?(name)>` default to a single non-empty path segment
/// (never crossing `/`), while `<?tree>` matches multi-segment subtrees.
pub fn compile_path_str(src: &str) -> Result<Pattern, CompileError> {
    compile_str_with_mode(src, CompileMode::Path)
}

/// Parses and compiles a Tomet pattern string under the given [`CompileMode`].
pub fn compile_str_with_mode(src: &str, mode: CompileMode) -> Result<Pattern, CompileError> {
    let doc = tomet_ast_parser::parse_file(src).map_err(|e| CompileError::new(e.to_string()))?;
    compile_blocks_with_mode(&doc.body, mode)
}

/// Compiles a slice of [`Block`]s into a [`Pattern`] in [`CompileMode::Text`] mode.
pub fn compile_blocks(blocks: &[Block]) -> Result<Pattern, CompileError> {
    compile_blocks_with_mode(blocks, CompileMode::Text)
}

/// Compiles a slice of [`Block`]s into a [`Pattern`] under the given [`CompileMode`].
pub fn compile_blocks_with_mode(blocks: &[Block], mode: CompileMode) -> Result<Pattern, CompileError> {
    if blocks.is_empty() {
        return Ok(Pattern::Literal(String::new()));
    }
    if blocks.len() == 1 {
        return compile_block_with_mode(&blocks[0], mode);
    }
    let mut parts = Vec::with_capacity(blocks.len() * 2 - 1);
    for (idx, block) in blocks.iter().enumerate() {
        if idx > 0 {
            parts.push(Pattern::Literal("\n".to_string()));
        }
        parts.push(compile_block_with_mode(block, mode)?);
    }
    Ok(Pattern::seq(parts))
}

/// Compiles a single [`Block`] into a [`Pattern`] in [`CompileMode::Text`] mode.
pub fn compile_block(block: &Block) -> Result<Pattern, CompileError> {
    compile_block_with_mode(block, CompileMode::Text)
}

/// Compiles a single [`Block`] into a [`Pattern`] under the given [`CompileMode`].
pub fn compile_block_with_mode(block: &Block, mode: CompileMode) -> Result<Pattern, CompileError> {
    match block {
        Block::Paragraph(p) => compile_inlines_with_mode(&p.content, mode),
        Block::Hole(hole) => compile_hole_with_mode(hole, mode),
        _ => Err(CompileError::new(
            "only paragraphs and holes are supported in text patterns",
        )),
    }
}

/// Compiles a sequence of [`Inline`]s into a [`Pattern`] in [`CompileMode::Text`] mode.
pub fn compile_inlines(inlines: &[Inline]) -> Result<Pattern, CompileError> {
    compile_inlines_with_mode(inlines, CompileMode::Text)
}

/// Compiles a sequence of [`Inline`]s into a [`Pattern`] under the given [`CompileMode`].
pub fn compile_inlines_with_mode(
    inlines: &[Inline],
    mode: CompileMode,
) -> Result<Pattern, CompileError> {
    let mut parts = Vec::with_capacity(inlines.len());
    for inline in inlines {
        match inline {
            Inline::Text(t) => parts.push(Pattern::Literal(t.value.clone())),
            Inline::SoftBreak(_) => parts.push(Pattern::Literal(" ".to_string())),
            Inline::Hole(hole) => parts.push(compile_hole_with_mode(hole, mode)?),
            _ => {
                return Err(CompileError::new(
                    "unsupported inline node inside hole pattern",
                ));
            }
        }
    }
    if parts.len() == 1 {
        Ok(parts.pop().unwrap())
    } else {
        Ok(Pattern::seq(parts))
    }
}

/// Compiles a [`tomet_ast::Hole`] (`<?matcher(name, args){options}>[content]`) into a [`Pattern`]
/// in [`CompileMode::Text`] mode.
pub fn compile_hole(hole: &Hole) -> Result<Pattern, CompileError> {
    compile_hole_with_mode(hole, CompileMode::Text)
}

/// Compiles a [`tomet_ast::Hole`] under the given [`CompileMode`].
pub fn compile_hole_with_mode(hole: &Hole, mode: CompileMode) -> Result<Pattern, CompileError> {
    let matcher_name = hole.matcher.as_ref().map(|m| m.name.as_str());
    let args = hole.data.args.as_ref();
    let opts = hole.data.value.as_ref();

    let (bind_name, inner) = match matcher_name {
        None => {
            let bind_name = take_no_positional_args(args, "<?>")?;
            let inner = match &hole.content {
                Some(blocks) => compile_blocks_with_mode(blocks, mode)?,
                None => match mode {
                    CompileMode::Text => {
                        let cross_newline = opt_bool(opts, "cross_newline")?.unwrap_or(false);
                        Pattern::many1(Pattern::AnyChar { cross_newline })
                    }
                    CompileMode::Path => Pattern::Builtin(BuiltinMatcher::Seg),
                },
            };
            (bind_name, inner)
        }
        Some("int") => {
            let bind_name = take_no_positional_args(args, "int")?;
            let min = opt_int(opts, "min")?;
            let max = opt_int(opts, "max")?;
            if let (Some(lo), Some(hi)) = (min, max) {
                if lo > hi {
                    return Err(CompileError::new(format!(
                        "`<?int>` `min` ({lo}) cannot exceed `max` ({hi})"
                    )));
                }
            }
            (bind_name, Pattern::Builtin(BuiltinMatcher::Int { min, max }))
        }
        Some("uint") => {
            let bind_name = take_no_positional_args(args, "uint")?;
            let min = opt_uint(opts, "min", "uint")?;
            let max = opt_uint(opts, "max", "uint")?;
            if let (Some(lo), Some(hi)) = (min, max) {
                if lo > hi {
                    return Err(CompileError::new(format!(
                        "`<?uint>` `min` ({lo}) cannot exceed `max` ({hi})"
                    )));
                }
            }
            (bind_name, Pattern::Builtin(BuiltinMatcher::Uint { min, max }))
        }
        Some("float") => {
            let bind_name = take_no_positional_args(args, "float")?;
            (bind_name, Pattern::Builtin(BuiltinMatcher::Float))
        }
        Some("bool") => {
            let bind_name = take_no_positional_args(args, "bool")?;
            (bind_name, Pattern::Builtin(BuiltinMatcher::Bool))
        }
        Some("version") => {
            let bind_name = take_no_positional_args(args, "version")?;
            (bind_name, Pattern::Builtin(BuiltinMatcher::Version))
        }
        Some("date") => {
            let bind_name = take_no_positional_args(args, "date")?;
            (bind_name, Pattern::Builtin(BuiltinMatcher::Date))
        }
        Some("datetime") => {
            let bind_name = take_no_positional_args(args, "datetime")?;
            (bind_name, Pattern::Builtin(BuiltinMatcher::Datetime))
        }
        Some("uuid") => {
            let bind_name = take_no_positional_args(args, "uuid")?;
            (bind_name, Pattern::Builtin(BuiltinMatcher::Uuid))
        }
        Some("nanoid") => {
            let bind_name = take_no_positional_args(args, "nanoid")?;
            let length = opt_usize(opts, "length", "nanoid")?;
            if length == Some(0) {
                return Err(CompileError::new("`<?nanoid>` `length` must be at least 1"));
            }
            (bind_name, Pattern::Builtin(BuiltinMatcher::Nanoid { length }))
        }
        Some("kebab") => {
            let bind_name = take_no_positional_args(args, "kebab")?;
            (bind_name, Pattern::Builtin(BuiltinMatcher::Case(CaseStyle::Kebab)))
        }
        Some("snake") => {
            let bind_name = take_no_positional_args(args, "snake")?;
            (bind_name, Pattern::Builtin(BuiltinMatcher::Case(CaseStyle::Snake)))
        }
        Some("camel") => {
            let bind_name = take_no_positional_args(args, "camel")?;
            (bind_name, Pattern::Builtin(BuiltinMatcher::Case(CaseStyle::Camel)))
        }
        Some("pascal") => {
            let bind_name = take_no_positional_args(args, "pascal")?;
            (bind_name, Pattern::Builtin(BuiltinMatcher::Case(CaseStyle::Pascal)))
        }
        Some("screaming_snake") => {
            let bind_name = take_no_positional_args(args, "screaming_snake")?;
            (
                bind_name,
                Pattern::Builtin(BuiltinMatcher::Case(CaseStyle::ScreamingSnake)),
            )
        }
        Some("slug") => {
            let bind_name = take_no_positional_args(args, "slug")?;
            (bind_name, Pattern::Builtin(BuiltinMatcher::Case(CaseStyle::Slug)))
        }
        Some("seg") => {
            let bind_name = take_no_positional_args(args, "seg")?;
            (bind_name, Pattern::Builtin(BuiltinMatcher::Seg))
        }
        Some("tree") => {
            let bind_name = take_no_positional_args(args, "tree")?;
            let min_depth = opt_usize(opts, "min_depth", "tree")?.unwrap_or(1);
            let max_depth = opt_usize(opts, "max_depth", "tree")?;
            if let Some(max) = max_depth {
                if min_depth > max {
                    return Err(CompileError::new(format!(
                        "`<?tree>` `min_depth` ({min_depth}) cannot exceed `max_depth` ({max})"
                    )));
                }
            }
            (
                bind_name,
                Pattern::Builtin(BuiltinMatcher::Tree {
                    min_depth,
                    max_depth,
                }),
            )
        }
        Some("digit") => {
            let bind_name = take_no_positional_args(args, "digit")?;
            (bind_name, Pattern::Class(CharClass::Digit))
        }
        Some("alpha") => {
            let bind_name = take_no_positional_args(args, "alpha")?;
            (bind_name, Pattern::Class(CharClass::Alpha))
        }
        Some("alnum") => {
            let bind_name = take_no_positional_args(args, "alnum")?;
            (bind_name, Pattern::Class(CharClass::Alnum))
        }
        Some("hex") => {
            let bind_name = take_no_positional_args(args, "hex")?;
            (bind_name, Pattern::Class(CharClass::Hex))
        }
        Some("ascii") => {
            let bind_name = take_no_positional_args(args, "ascii")?;
            (bind_name, Pattern::Class(CharClass::Ascii))
        }
        Some("space") => {
            let bind_name = take_no_positional_args(args, "space")?;
            (bind_name, Pattern::Class(CharClass::Space))
        }
        Some("non_space") => {
            let bind_name = take_no_positional_args(args, "non_space")?;
            (bind_name, Pattern::Class(CharClass::NonSpace))
        }
        Some("word") => {
            let bind_name = take_no_positional_args(args, "word")?;
            (bind_name, Pattern::Class(CharClass::Word))
        }
        Some("chars") => {
            let (bind_name, pos) = take_exact_positional_args(args, "chars", 1)?;
            let set = require_text_arg(pos[0], "chars", "character set string")?;
            (bind_name, Pattern::chars(set))
        }
        Some("not_chars") => {
            let (bind_name, pos) = take_exact_positional_args(args, "not_chars", 1)?;
            let set = require_text_arg(pos[0], "not_chars", "character set string")?;
            (bind_name, Pattern::not_chars(set))
        }
        Some("range") => {
            let (bind_name, pos) = take_exact_positional_args(args, "range", 2)?;
            let start = require_char_arg(pos[0], "range", "start")?;
            let end = require_char_arg(pos[1], "range", "end")?;
            if start > end {
                return Err(CompileError::new(format!(
                    "`<?range>` start `{start}` cannot exceed end `{end}`"
                )));
            }
            (bind_name, Pattern::range(start, end))
        }
        Some("opt") => {
            let (bind_name, body) = resolve_inner_pattern(hole, args, "opt", mode)?;
            (bind_name, Pattern::opt(body))
        }
        Some("many") => {
            let (bind_name, body) = resolve_inner_pattern(hole, args, "many", mode)?;
            let (min, max, greedy) = read_repeat_opts(opts, 0, "many")?;
            (bind_name, Pattern::repeat(body, min, max, greedy))
        }
        Some("many1") => {
            let (bind_name, body) = resolve_inner_pattern(hole, args, "many1", mode)?;
            let (min, max, greedy) = read_repeat_opts(opts, 1, "many1")?;
            if min < 1 {
                return Err(CompileError::new("`<?many1>` `min` must be at least 1"));
            }
            (bind_name, Pattern::repeat(body, min, max, greedy))
        }
        Some("sep_by") => compile_sep_by(hole, args, opts, mode)?,
        Some("alt") => compile_alt(args, mode)?,
        Some("not") => {
            let (bind_name, body) = resolve_inner_pattern(hole, args, "not", mode)?;
            (bind_name, Pattern::not_followed_by(body))
        }
        Some("not_followed_by") => {
            let (bind_name, body) = resolve_inner_pattern(hole, args, "not_followed_by", mode)?;
            (bind_name, Pattern::not_followed_by(body))
        }
        Some("followed_by") => {
            let (bind_name, body) = resolve_inner_pattern(hole, args, "followed_by", mode)?;
            (bind_name, Pattern::followed_by(body))
        }
        Some("balanced") => {
            let bind_name = take_no_positional_args(args, "balanced")?;
            let open = opt_text(opts, "open")?.unwrap_or_else(|| "(".to_string());
            let close = opt_text(opts, "close")?.unwrap_or_else(|| ")".to_string());
            if open.is_empty() || close.is_empty() {
                return Err(CompileError::new(
                    "`<?balanced>` `open` and `close` delimiters must be non-empty",
                ));
            }
            (bind_name, Pattern::Balanced { open, close })
        }
        Some(other) => {
            return Err(CompileError::new(format!(
                "unknown hole matcher `{other}`"
            )));
        }
    };

    match bind_name {
        Some(name) => Ok(inner.capture(Some(name))),
        None => Ok(inner),
    }
}

fn read_repeat_opts(
    opts: Option<&Group>,
    default_min: usize,
    matcher: &str,
) -> Result<(usize, Option<usize>, bool), CompileError> {
    let min = opt_usize(opts, "min", matcher)?.unwrap_or(default_min);
    let max = opt_usize(opts, "max", matcher)?;
    if let Some(m) = max {
        if min > m {
            return Err(CompileError::new(format!(
                "`<?{matcher}>` `min` ({min}) cannot exceed `max` ({m})"
            )));
        }
    }
    let greedy = opt_bool(opts, "greedy")?.unwrap_or(true);
    Ok((min, max, greedy))
}

fn compile_sep_by(
    hole: &Hole,
    args: Option<&Tuple>,
    opts: Option<&Group>,
    mode: CompileMode,
) -> Result<(Option<String>, Pattern), CompileError> {
    let (min, max, greedy) = read_repeat_opts(opts, 0, "sep_by")?;
    let (bind_name, item, sep) = if let Some(blocks) = hole.content.as_deref() {
        let (bind_name, pos) = take_exact_positional_args(args, "sep_by", 1)?;
        let item = compile_blocks_with_mode(blocks, mode)?;
        let sep = compile_value_pattern(pos[0], mode)?;
        (bind_name, item, sep)
    } else {
        let (bind_name, pos) = take_exact_positional_args(args, "sep_by", 2)?;
        let item = compile_value_pattern(pos[0], mode)?;
        let sep = compile_value_pattern(pos[1], mode)?;
        (bind_name, item, sep)
    };
    Ok((bind_name, Pattern::sep_by_range(item, sep, min, max, greedy)))
}

fn resolve_inner_pattern(
    hole: &Hole,
    args: Option<&Tuple>,
    matcher: &str,
    mode: CompileMode,
) -> Result<(Option<String>, Pattern), CompileError> {
    if let Some(blocks) = hole.content.as_deref() {
        let bind_name = take_no_positional_args(args, matcher)?;
        let body = compile_blocks_with_mode(blocks, mode)?;
        return Ok((bind_name, body));
    }

    let unnamed = unnamed_entries(args);
    match unnamed.as_slice() {
        [val] if matches!(val, Value::Blocks(_) | Value::Hole(_)) => {
            Ok((None, compile_value_pattern(val, mode)?))
        }
        [first, val] if matches!(val, Value::Blocks(_) | Value::Hole(_)) => {
            let bind_name = parse_bind_name_value(first, matcher)?;
            Ok((Some(bind_name), compile_value_pattern(val, mode)?))
        }
        _ => Err(CompileError::new(format!(
            "`<?{matcher}>` requires `[content]`"
        ))),
    }
}

fn compile_alt(args: Option<&Tuple>, mode: CompileMode) -> Result<(Option<String>, Pattern), CompileError> {
    let unnamed = unnamed_entries(args);
    let (bind_name, items): (Option<String>, Vec<&Value>) = match unnamed.as_slice() {
        [Value::Tuple(tuple)] => (None, tuple.values().collect()),
        [first, Value::Tuple(tuple)] => {
            let name = parse_bind_name_value(first, "alt")?;
            (Some(name), tuple.values().collect())
        }
        items if items.len() >= 2 && matches!(items[0], Value::Blocks(_) | Value::Hole(_)) => {
            (None, items.to_vec())
        }
        items
            if items.len() >= 3
                && matches!(items[1], Value::Blocks(_) | Value::Hole(_)) =>
        {
            let name = parse_bind_name_value(items[0], "alt")?;
            (Some(name), items[1..].to_vec())
        }
        _ => {
            return Err(CompileError::new(
                "`<?alt(...)>` requires a tuple of pattern branches",
            ));
        }
    };

    let mut branches = Vec::with_capacity(items.len());
    for item in items {
        branches.push(compile_value_pattern(item, mode)?);
    }
    Ok((bind_name, Pattern::alt(branches)))
}

fn unnamed_entries(args: Option<&Tuple>) -> Vec<&Value> {
    let Some(tuple) = args else {
        return Vec::new();
    };
    tuple
        .entries()
        .filter(|e| e.name().is_none())
        .map(|e| e.value())
        .collect()
}

fn take_no_positional_args(
    args: Option<&Tuple>,
    matcher: &str,
) -> Result<Option<String>, CompileError> {
    let unnamed = unnamed_entries(args);
    match unnamed.as_slice() {
        [] => Ok(None),
        [first] => parse_bind_name_value(first, matcher).map(Some),
        _ => Err(CompileError::new(format!(
            "`{matcher}` takes at most one binding name"
        ))),
    }
}

fn take_exact_positional_args<'a>(
    args: Option<&'a Tuple>,
    matcher: &str,
    count: usize,
) -> Result<(Option<String>, Vec<&'a Value>), CompileError> {
    let unnamed = unnamed_entries(args);
    if unnamed.len() == count {
        Ok((None, unnamed))
    } else if unnamed.len() == count + 1 {
        let name = parse_bind_name_value(unnamed[0], matcher)?;
        Ok((Some(name), unnamed[1..].to_vec()))
    } else {
        Err(CompileError::new(format!(
            "`<?{matcher}>` expects {count} positional argument(s) (plus optional binding name), got {}",
            unnamed.len()
        )))
    }
}

fn parse_bind_name_value(val: &Value, matcher: &str) -> Result<String, CompileError> {
    match val {
        Value::Scalar(Scalar::Text(s)) if is_bind_name(s) => Ok(s.clone()),
        Value::Scalar(Scalar::Text(s)) => Err(CompileError::new(format!(
            "`{s}` is not a valid binding name in `{matcher}`"
        ))),
        _ => Err(CompileError::new(format!(
            "binding name in `{matcher}` must be an identifier"
        ))),
    }
}

fn is_bind_name(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_alphanumeric() || c == '_')
}

fn require_text_arg(val: &Value, matcher: &str, what: &str) -> Result<String, CompileError> {
    match val {
        Value::Scalar(Scalar::Text(s)) => Ok(s.clone()),
        _ => Err(CompileError::new(format!(
            "`<?{matcher}>` {what} must be a string"
        ))),
    }
}

fn require_char_arg(val: &Value, matcher: &str, label: &str) -> Result<char, CompileError> {
    let s = require_text_arg(val, matcher, label)?;
    let mut chars = s.chars();
    match (chars.next(), chars.next()) {
        (Some(ch), None) => Ok(ch),
        _ => Err(CompileError::new(format!(
            "`<?{matcher}>` {label} must be a single character, got `{s}`"
        ))),
    }
}

fn compile_value_pattern(val: &Value, mode: CompileMode) -> Result<Pattern, CompileError> {
    match val {
        Value::Blocks(blocks) => compile_blocks_with_mode(blocks, mode),
        Value::Hole(hole) => compile_hole_with_mode(hole, mode),
        Value::Scalar(Scalar::Text(s)) => Ok(Pattern::Literal(s.clone())),
        _ => Err(CompileError::new(
            "expected `[pattern]`, `<?hole>`, or string inside pattern argument",
        )),
    }
}

fn opt_value<'a>(group: Option<&'a Group>, key: &str) -> Option<&'a Value> {
    let group = group?;
    group.0.iter().find_map(|e| match e {
        tomet_ast::Entry::Pair(k, v) if k == key => Some(v),
        _ => None,
    })
}

fn opt_int(group: Option<&Group>, key: &str) -> Result<Option<i64>, CompileError> {
    let Some(val) = opt_value(group, key) else {
        return Ok(None);
    };
    match val {
        Value::Scalar(Scalar::Int(n)) => Ok(Some(*n)),
        _ => Err(CompileError::new(format!("option `{key}` must be an integer"))),
    }
}

fn opt_uint(group: Option<&Group>, key: &str, matcher: &str) -> Result<Option<u64>, CompileError> {
    let Some(val) = opt_int(group, key)? else {
        return Ok(None);
    };
    u64::try_from(val).map(Some).map_err(|_| {
        CompileError::new(format!(
            "`<?{matcher}>` option `{key}` must be non-negative, got {val}"
        ))
    })
}

fn opt_usize(group: Option<&Group>, key: &str, matcher: &str) -> Result<Option<usize>, CompileError> {
    let Some(val) = opt_int(group, key)? else {
        return Ok(None);
    };
    usize::try_from(val).map(Some).map_err(|_| {
        CompileError::new(format!(
            "`<?{matcher}>` option `{key}` must be a non-negative integer, got {val}"
        ))
    })
}

fn opt_bool(group: Option<&Group>, key: &str) -> Result<Option<bool>, CompileError> {
    let Some(val) = opt_value(group, key) else {
        return Ok(None);
    };
    match val {
        Value::Scalar(Scalar::Bool(b)) => Ok(Some(*b)),
        _ => Err(CompileError::new(format!("option `{key}` must be a boolean"))),
    }
}

fn opt_text(group: Option<&Group>, key: &str) -> Result<Option<String>, CompileError> {
    let Some(val) = opt_value(group, key) else {
        return Ok(None);
    };
    match val {
        Value::Scalar(Scalar::Text(s)) => Ok(Some(s.clone())),
        _ => Err(CompileError::new(format!("option `{key}` must be a string"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hole_engine::{Program, Template};

    #[test]
    fn parses_hole_spec_example() {
        let pat = compile_str("adr-<?int(n)>-<?kebab(slug)>.md").unwrap();
        let prog = Program::compile(&pat);
        let m = prog.match_full("adr-012-zone-design.md").unwrap();
        assert_eq!(m.get("n").unwrap().text, "012");
        assert_eq!(m.get("slug").unwrap().text, "zone-design");
        assert!(prog.match_full("adr-012-BadName.md").is_none());
    }

    #[test]
    fn parses_nested_capture_group_and_opt() {
        let pat = compile_str("<?(id)>[adr-<?int(n)><?opt>[-<?kebab(slug)>]]").unwrap();
        let prog = Program::compile(&pat);

        let m1 = prog.match_full("adr-7-hello-world").unwrap();
        assert_eq!(m1.get("id").unwrap().text, "adr-7-hello-world");
        assert_eq!(m1.get("n").unwrap().text, "7");
        assert_eq!(m1.get("slug").unwrap().text, "hello-world");

        let m2 = prog.match_full("adr-7").unwrap();
        assert_eq!(m2.get("id").unwrap().text, "adr-7");
        assert_eq!(m2.get("n").unwrap().text, "7");
        assert!(m2.get("slug").is_none());
    }

    #[test]
    fn parses_alt_branches() {
        let pat = compile_str("<?alt(doc, ([adr-<?int(n)>], [rfc-<?int(n)>]))>").unwrap();
        let prog = Program::compile(&pat);
        let m = prog.match_full("rfc-42").unwrap();
        assert_eq!(m.get("doc").unwrap().text, "rfc-42");
        assert_eq!(m.get("n").unwrap().text, "42");
    }

    #[test]
    fn path_mode_scopes_unnamed_holes_to_single_segment() {
        let pat = compile_path_str("crates/<?(pkg)>/src/<?(file)>.rs").unwrap();
        let prog = Program::compile(&pat);

        let m = prog.match_full("crates/hole-engine/src/vm.rs").unwrap();
        assert_eq!(m.get("pkg").unwrap().text, "hole-engine");
        assert_eq!(m.get("file").unwrap().text, "vm");

        // `<?>` in path mode must not cross `/`
        assert!(prog.match_full("crates/a/b/src/vm.rs").is_none());
        assert!(prog.match_full("crates/hole-engine/src/a/vm.rs").is_none());
        assert!(prog.match_full("crates//src/vm.rs").is_none());
    }

    #[test]
    fn path_mode_tree_crosses_slashes_with_depth_bounds() {
        let pat = compile_path_str("tmtroot/<?tree(sub)>.tmt").unwrap();
        let prog = Program::compile(&pat);
        assert_eq!(
            prog.match_full("tmtroot/intro.tmt").unwrap().get("sub").unwrap().text,
            "intro"
        );
        assert_eq!(
            prog.match_full("tmtroot/spec/forms/hole.tmt")
                .unwrap()
                .get("sub")
                .unwrap()
                .text,
            "spec/forms/hole"
        );
        // Clean segments only: no leading/trailing `/` or empty segments `//`
        assert!(prog.match_full("tmtroot//hole.tmt").is_none());
        assert!(prog.match_full("tmtroot/.tmt").is_none());

        let bounded = compile_path_str("<?tree(p){min_depth: 2, max_depth: 3}>").unwrap();
        let bprog = Program::compile(&bounded);
        assert!(bprog.match_full("a").is_none());
        assert!(bprog.match_full("a/b").is_some());
        assert!(bprog.match_full("a/b/c").is_some());
        assert!(bprog.match_full("a/b/c/d").is_none());
        assert!(bprog.match_full("/a/b").is_none());
        assert!(bprog.match_full("a/b/").is_none());
        assert!(bprog.match_full("a//b").is_none());
    }

    #[test]
    fn compiles_remaining_base_types() {
        let pat = compile_str(
            "f=<?float(f)> b=<?bool(b)> dt=<?datetime(dt)> id=<?nanoid(id)> short=<?nanoid(s){length: 8}>",
        )
        .unwrap();
        let prog = Program::compile(&pat);

        let m = prog
            .match_full(
                "f=-12.5e3 b=true dt=2026-11-18T09:30:00+09:00 id=V1StGXR8_Z5jdHi6B-myT short=abc123_-",
            )
            .unwrap();
        assert_eq!(m.get("f").unwrap().text, "-12.5e3");
        assert_eq!(m.get("b").unwrap().text, "true");
        assert_eq!(m.get("dt").unwrap().text, "2026-11-18T09:30:00+09:00");
        assert_eq!(m.get("id").unwrap().text, "V1StGXR8_Z5jdHi6B-myT");
        assert_eq!(m.get("s").unwrap().text, "abc123_-");

        // Wrong nanoid length is rejected
        assert!(
            prog.match_full("f=1 b=false dt=2026-11-18 id=too_short short=abc123_-")
                .is_none()
        );
    }

    #[test]
    fn compiles_char_classes_and_combinators() {
        let pat = compile_str(
            "<?range(ch, \"a\", \"f\")><?chars(v, \"aeiou\")><?not_chars(x, \"0123456789\")>",
        )
        .unwrap();
        let prog = Program::compile(&pat);
        let m = prog.match_full("aez").unwrap();
        assert_eq!(m.get("ch").unwrap().text, "a");
        assert_eq!(m.get("v").unwrap().text, "e");
        assert_eq!(m.get("x").unwrap().text, "z");
        assert!(prog.match_full("ae5").is_none());

        // Unnamed char classes must not mistake the char argument for a binding name
        let anon = compile_str("<?chars(\"abc\")>-<?range(\"0\", \"9\")>").unwrap();
        let aprog = Program::compile(&anon);
        assert!(aprog.is_full_match("b-7"));
        assert!(!aprog.is_full_match("d-7"));
    }

    #[test]
    fn compiles_sep_by_and_lookaheads() {
        let pat = compile_str("[<?sep_by(items, [<?int>], \", \")>]").unwrap();
        let prog = Program::compile(&pat);
        assert_eq!(
            prog.match_full("[1, 2, -3]")
                .unwrap()
                .get("items")
                .unwrap()
                .text,
            "1, 2, -3"
        );
        assert_eq!(prog.match_full("[]").unwrap().get("items").unwrap().text, "");
        assert!(prog.match_full("[1, 2,]").is_none());

        // Content form of sep_by with distinct captured items
        let pat_content = compile_str("<?sep_by(\", \")>[<?int(n)>]").unwrap();
        let m_items = Program::compile(&pat_content).match_full("10, 20, 30").unwrap();
        let vals: Vec<&str> = m_items
            .get_all("n")
            .into_iter()
            .map(|c| c.text.as_str())
            .collect();
        assert_eq!(vals, vec!["10", "20", "30"]);

        let la = compile_str(
            "<?kebab(name)><?followed_by>[@<?version>]@<?not_followed_by>[0.0.0]<?version(v)>",
        )
        .unwrap();
        let lprog = Program::compile(&la);
        let m = lprog.match_full("catasto@1.2.3").unwrap();
        assert_eq!(m.get("name").unwrap().text, "catasto");
        assert_eq!(m.get("v").unwrap().text, "1.2.3");
        assert!(lprog.match_full("catasto@0.0.0").is_none());

        let many1_prog = Program::compile(&compile_str("<?many1(xs)>[a]").unwrap());
        assert!(!many1_prog.is_full_match(""));
        assert_eq!(many1_prog.match_full("aaa").unwrap().get("xs").unwrap().text, "aaa");

        let seg_in_text = Program::compile(&compile_str("<?seg(dir)>/<?seg(file)>").unwrap());
        let sm = seg_in_text.match_full("crates/tomet-hole").unwrap();
        assert_eq!(sm.get("dir").unwrap().text, "crates");
        assert_eq!(sm.get("file").unwrap().text, "tomet-hole");
        assert!(seg_in_text.match_full("a/b/c").is_none());
    }

    #[test]
    fn repeated_binding_equality_and_template_expansion() {
        let pat = compile_str("<?(x)>-<?(x)>").unwrap();
        let prog = Program::compile(&pat);
        let m = prog.match_full("ab-ab").unwrap();
        assert_eq!(m.get("x").unwrap().text, "ab");
        assert!(prog.match_full("ab-cd").is_none());

        // End-to-end with template expansion
        let p2 = compile_path_str("src/<?(p)>/<?int(n)>.rs").unwrap();
        let m2 = Program::compile(&p2).match_full("src/vm/42.rs").unwrap();
        let tpl = Template::parse("tests/${hole.p}/${hole.n}_test.rs").unwrap();
        assert_eq!(m2.expand(&tpl).unwrap(), "tests/vm/42_test.rs");
    }

    #[test]
    fn rejects_invalid_options_and_arguments() {
        assert!(compile_str("<?nanoid{length: 0}>").is_err());
        assert!(compile_str("<?uint{min: -1}>").is_err());
        assert!(compile_str("<?tree{min_depth: 3, max_depth: 1}>").is_err());
        assert!(compile_str("<?range(\"z\", \"a\")>").is_err());
        assert!(compile_str("<?range(\"ab\", \"z\")>").is_err());
        assert!(compile_str("<?many(items)>").is_err());
    }
}
