//! Bridge between `tomet-ast` `Hole` (`<?...>`) nodes and `hole-engine`.

use std::fmt;

use hole_engine::{BuiltinMatcher, CaseStyle, CharClass, Pattern};
use tomet_ast::{Block, Hole, Inline, Tuple, Value};
use tomet_scalar::Scalar;

pub use hole_engine;

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

/// Compiles a slice of [`Block`]s (for example from a `[...]` pattern value or hole body) into a [`Pattern`].
pub fn compile_blocks(blocks: &[Block]) -> Result<Pattern, CompileError> {
    if blocks.is_empty() {
        return Ok(Pattern::Literal(String::new()));
    }
    if blocks.len() == 1 {
        return compile_block(&blocks[0]);
    }
    let mut parts = Vec::with_capacity(blocks.len());
    for (idx, block) in blocks.iter().enumerate() {
        if idx > 0 {
            parts.push(Pattern::Literal("\n".to_string()));
        }
        parts.push(compile_block(block)?);
    }
    Ok(Pattern::seq(parts))
}

/// Compiles a single [`Block`] into a [`Pattern`].
pub fn compile_block(block: &Block) -> Result<Pattern, CompileError> {
    match block {
        Block::Paragraph(p) => compile_inlines(&p.content),
        Block::Hole(hole) => compile_hole(hole),
        _ => Err(CompileError::new(
            "only paragraphs and holes are supported in text patterns",
        )),
    }
}

/// Compiles a sequence of [`Inline`]s into a [`Pattern`].
pub fn compile_inlines(inlines: &[Inline]) -> Result<Pattern, CompileError> {
    let mut parts = Vec::with_capacity(inlines.len());
    for inline in inlines {
        match inline {
            Inline::Text(t) => parts.push(Pattern::Literal(t.value.clone())),
            Inline::SoftBreak(_) => parts.push(Pattern::Literal(" ".to_string())),
            Inline::Hole(hole) => parts.push(compile_hole(hole)?),
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

/// Compiles a [`tomet_ast::Hole`] (`<?matcher(name, args){options}>[content]`) into a [`Pattern`].
pub fn compile_hole(hole: &Hole) -> Result<Pattern, CompileError> {
    let matcher_name = hole.matcher.as_ref().map(|m| m.name.as_str());
    let (bind_name, extra_args) = split_args(hole.data.args.as_ref());
    let opts = hole.data.value.as_ref();

    let inner = match matcher_name {
        None => match &hole.content {
            Some(blocks) => compile_blocks(blocks)?,
            None => Pattern::many1(Pattern::AnyChar {
                cross_newline: false,
            }),
        },
        Some("int") => {
            let min = opt_int(opts, "min")?;
            let max = opt_int(opts, "max")?;
            Pattern::Builtin(BuiltinMatcher::Int { min, max })
        }
        Some("uint") => {
            let min = opt_int(opts, "min")?.map(|v| v.max(0) as u64);
            let max = opt_int(opts, "max")?.map(|v| v.max(0) as u64);
            Pattern::Builtin(BuiltinMatcher::Uint { min, max })
        }
        Some("version") => Pattern::Builtin(BuiltinMatcher::Version),
        Some("date") => Pattern::Builtin(BuiltinMatcher::Date),
        Some("uuid") => Pattern::Builtin(BuiltinMatcher::Uuid),
        Some("kebab") => Pattern::Builtin(BuiltinMatcher::Case(CaseStyle::Kebab)),
        Some("snake") => Pattern::Builtin(BuiltinMatcher::Case(CaseStyle::Snake)),
        Some("camel") => Pattern::Builtin(BuiltinMatcher::Case(CaseStyle::Camel)),
        Some("pascal") => Pattern::Builtin(BuiltinMatcher::Case(CaseStyle::Pascal)),
        Some("screaming_snake") => Pattern::Builtin(BuiltinMatcher::Case(CaseStyle::ScreamingSnake)),
        Some("slug") => Pattern::Builtin(BuiltinMatcher::Case(CaseStyle::Slug)),
        Some("seg") => Pattern::Builtin(BuiltinMatcher::Seg),
        Some("digit") => Pattern::Class(CharClass::Digit),
        Some("alpha") => Pattern::Class(CharClass::Alpha),
        Some("alnum") => Pattern::Class(CharClass::Alnum),
        Some("hex") => Pattern::Class(CharClass::Hex),
        Some("space") => Pattern::Class(CharClass::Space),
        Some("word") => Pattern::Class(CharClass::Word),
        Some("opt") => {
            let body = require_content_pattern(hole, "opt")?;
            Pattern::opt(body)
        }
        Some("many") => {
            let body = require_content_pattern(hole, "many")?;
            let min = opt_int(opts, "min")?.unwrap_or(0).max(0) as usize;
            let max = opt_int(opts, "max")?.map(|v| v.max(0) as usize);
            let greedy = opt_bool(opts, "greedy")?.unwrap_or(true);
            Pattern::Repeat {
                inner: Box::new(body),
                min,
                max,
                greedy,
            }
        }
        Some("alt") => compile_alt(&extra_args, hole)?,
        Some("not") => {
            let body = require_content_pattern(hole, "not")?;
            Pattern::Not(Box::new(body))
        }
        Some("balanced") => {
            let open = opt_text(opts, "open")?.unwrap_or_else(|| "(".to_string());
            let close = opt_text(opts, "close")?.unwrap_or_else(|| ")".to_string());
            Pattern::Balanced { open, close }
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

fn require_content_pattern(hole: &Hole, name: &str) -> Result<Pattern, CompileError> {
    let Some(blocks) = hole.content.as_deref() else {
        return Err(CompileError::new(format!(
            "`<?{name}>` requires `[content]`"
        )));
    };
    compile_blocks(blocks)
}

fn split_args(args: Option<&Tuple>) -> (Option<String>, Vec<&Value>) {
    let Some(tuple) = args else {
        return (None, Vec::new());
    };
    let mut unnamed: Vec<&Value> = tuple
        .entries()
        .filter(|e| e.name().is_none())
        .map(|e| e.value())
        .collect();
    if let Some(Value::Scalar(Scalar::Text(s))) = unnamed.first().copied() {
        if is_bind_name(s) {
            let name = s.clone();
            unnamed.remove(0);
            return (Some(name), unnamed);
        }
    }
    (None, unnamed)
}

fn is_bind_name(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_alphanumeric() || c == '_')
}

fn compile_alt(extra_args: &[&Value], hole: &Hole) -> Result<Pattern, CompileError> {
    if extra_args.is_empty() {
        return Err(CompileError::new(
            "`<?alt(...)>` requires a tuple of pattern branches",
        ));
    }
    let items: Vec<&Value> = match extra_args[0] {
        Value::Tuple(tuple) => tuple.values().collect(),
        other => vec![other],
    };
    let mut branches = Vec::with_capacity(items.len());
    for item in items {
        branches.push(compile_value_pattern(item)?);
    }
    let _ = hole;
    Ok(Pattern::alt(branches))
}

fn compile_value_pattern(val: &Value) -> Result<Pattern, CompileError> {
    match val {
        Value::Blocks(blocks) => compile_blocks(blocks),
        Value::Hole(hole) => compile_hole(hole),
        Value::Scalar(Scalar::Text(s)) => Ok(Pattern::Literal(s.clone())),
        _ => Err(CompileError::new(
            "expected `[pattern]`, `<?hole>`, or string inside `<?alt(...)>`",
        )),
    }
}

fn opt_value<'a>(group: Option<&'a tomet_ast::Group>, key: &str) -> Option<&'a Value> {
    let group = group?;
    group.0.iter().find_map(|e| match e {
        tomet_ast::Entry::Pair(k, v) if k == key => Some(v),
        _ => None,
    })
}

fn opt_int(group: Option<&tomet_ast::Group>, key: &str) -> Result<Option<i64>, CompileError> {
    let Some(val) = opt_value(group, key) else {
        return Ok(None);
    };
    match val {
        Value::Scalar(Scalar::Int(n)) => Ok(Some(*n)),
        _ => Err(CompileError::new(format!("option `{key}` must be an integer"))),
    }
}

fn opt_bool(group: Option<&tomet_ast::Group>, key: &str) -> Result<Option<bool>, CompileError> {
    let Some(val) = opt_value(group, key) else {
        return Ok(None);
    };
    match val {
        Value::Scalar(Scalar::Bool(b)) => Ok(Some(*b)),
        _ => Err(CompileError::new(format!("option `{key}` must be a boolean"))),
    }
}

fn opt_text(group: Option<&tomet_ast::Group>, key: &str) -> Result<Option<String>, CompileError> {
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
    use hole_engine::Program;

    fn parse_pattern(src: &str) -> Pattern {
        let doc = tomet_ast_parser::parse_file(src).expect("valid tomet");
        compile_blocks(&doc.body).expect("valid pattern")
    }

    #[test]
    fn parses_hole_spec_example() {
        let pat = parse_pattern("adr-<?int(n)>-<?kebab(slug)>.md");
        let prog = Program::compile(&pat);
        let m = prog.match_full("adr-012-zone-design.md").unwrap();
        assert_eq!(m.get("n").unwrap().text, "012");
        assert_eq!(m.get("slug").unwrap().text, "zone-design");
        assert!(prog.match_full("adr-012-BadName.md").is_none());
    }

    #[test]
    fn parses_nested_capture_group_and_opt() {
        let pat = parse_pattern("<?(id)>[adr-<?int(n)><?opt>[-<?kebab(slug)>]]");
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
        let pat = parse_pattern("<?alt(doc, ([adr-<?int(n)>], [rfc-<?int(n)>]))>");
        let prog = Program::compile(&pat);
        let m = prog.match_full("rfc-42").unwrap();
        assert_eq!(m.get("doc").unwrap().text, "rfc-42");
        assert_eq!(m.get("n").unwrap().text, "42");
    }
}
