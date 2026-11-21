//! Bytecode compiler from [`Pattern`] IR to a flat [`Program`].

use crate::pattern::{BuiltinMatcher, CharClass, Pattern};

/// A single bytecode instruction for the `hole-engine` VM.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Op {
    Literal(String),
    AnyChar {
        cross_newline: bool,
    },
    Class(CharClass),
    Builtin(BuiltinMatcher),
    Balanced {
        open: String,
        close: String,
    },
    StartAnchor,
    EndAnchor,
    WordBoundary,
    /// Unconditional jump to `pc`.
    Jump(usize),
    /// Push a backtrack frame pointing to `pc` and continue to `pc + 1`.
    Choice(usize),
    /// Pop the top backtrack frame and jump to `pc`.
    Commit(usize),
    /// Pop the top backtrack frame and continue to `pc + 1`.
    DropChoice,
    /// Begin a capture group at the current byte position with a distinct pattern site ID.
    OpenCapture {
        name: Option<String>,
        site: usize,
    },
    /// End the most recently opened capture group.
    CloseCapture,
    /// Begin a negative lookahead (`Not`): if body succeeds, fail outer via `NotFail`.
    NotStart(usize),
    /// Succeeds negative lookahead body -> fail the outer `Not`.
    NotFail,
    /// Begin a positive lookahead: saves position, body runs, restores position on `LookaheadEnd`.
    LookaheadStart(usize),
    /// Restores saved position after positive lookahead body succeeds.
    LookaheadEnd,
    /// Successful match termination.
    Match,
}

/// A compiled pattern program ready to execute on the VM.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Program {
    pub(crate) ops: Vec<Op>,
}

impl Program {
    /// Compiles a [`Pattern`] into a [`Program`].
    pub fn compile(pattern: &Pattern) -> Self {
        let mut ctx = CompileCtx::default();
        ctx.compile(pattern);
        ctx.ops.push(Op::Match);
        Self { ops: ctx.ops }
    }

    /// Returns the compiled bytecode instructions.
    pub fn ops(&self) -> &[Op] {
        &self.ops
    }
}

#[derive(Default)]
struct CompileCtx {
    ops: Vec<Op>,
    next_site: usize,
}

impl CompileCtx {
    fn alloc_site(&mut self) -> usize {
        let id = self.next_site;
        self.next_site += 1;
        id
    }

    fn emit_fail(&mut self) {
        let after = self.ops.len() + 2;
        self.ops.push(Op::NotStart(after));
        self.ops.push(Op::NotFail);
    }

    fn compile(&mut self, pattern: &Pattern) {
        match pattern {
            Pattern::Literal(s) => {
                if !s.is_empty() {
                    self.ops.push(Op::Literal(s.clone()));
                }
            }
            Pattern::AnyChar { cross_newline } => {
                self.ops.push(Op::AnyChar {
                    cross_newline: *cross_newline,
                });
            }
            Pattern::Class(class) => {
                self.ops.push(Op::Class(class.clone()));
            }
            Pattern::Builtin(b) => match b {
                BuiltinMatcher::Seg => {
                    let seg = Pattern::many1(Pattern::not_chars("/"));
                    self.compile(&seg);
                }
                BuiltinMatcher::Tree {
                    min_depth,
                    max_depth,
                } => {
                    let seg = Pattern::many1(Pattern::not_chars("/"));
                    let tree =
                        Pattern::sep_by_range(seg, Pattern::lit("/"), *min_depth, *max_depth, true);
                    self.compile(&tree);
                }
                _ => self.ops.push(Op::Builtin(b.clone())),
            },
            Pattern::Balanced { open, close } => {
                self.ops.push(Op::Balanced {
                    open: open.clone(),
                    close: close.clone(),
                });
            }
            Pattern::Start => self.ops.push(Op::StartAnchor),
            Pattern::End => self.ops.push(Op::EndAnchor),
            Pattern::WordBoundary => self.ops.push(Op::WordBoundary),
            Pattern::Seq(parts) => {
                for part in parts {
                    self.compile(part);
                }
            }
            Pattern::Capture { name, inner } => {
                let site = self.alloc_site();
                self.ops.push(Op::OpenCapture {
                    name: name.clone(),
                    site,
                });
                self.compile(inner);
                self.ops.push(Op::CloseCapture);
            }
            Pattern::Alt(branches) => self.compile_alt(branches),
            Pattern::Repeat {
                inner,
                min,
                max,
                greedy,
            } => self.compile_repeat(inner, *min, *max, *greedy),
            Pattern::SepBy {
                item,
                sep,
                min,
                max,
                greedy,
            } => self.compile_sep_by(item, sep, *min, *max, *greedy),
            Pattern::Not(inner) => {
                let not_idx = self.ops.len();
                self.ops.push(Op::NotStart(0));
                self.compile(inner);
                self.ops.push(Op::NotFail);
                let after_pc = self.ops.len();
                self.ops[not_idx] = Op::NotStart(after_pc);
            }
            Pattern::Lookahead(inner) => {
                let la_idx = self.ops.len();
                self.ops.push(Op::LookaheadStart(0));
                self.compile(inner);
                self.ops.push(Op::LookaheadEnd);
                let after_pc = self.ops.len();
                self.ops[la_idx] = Op::LookaheadStart(after_pc);
            }
        }
    }

    fn compile_alt(&mut self, branches: &[Pattern]) {
        if branches.is_empty() {
            self.emit_fail();
            return;
        }
        if branches.len() == 1 {
            self.compile(&branches[0]);
            return;
        }
        let mut commit_jumps = Vec::with_capacity(branches.len() - 1);
        for (i, branch) in branches.iter().enumerate() {
            let is_last = i + 1 == branches.len();
            if is_last {
                self.compile(branch);
            } else {
                let choice_idx = self.ops.len();
                self.ops.push(Op::Choice(0));
                self.compile(branch);
                let commit_idx = self.ops.len();
                self.ops.push(Op::Commit(0));
                commit_jumps.push(commit_idx);
                let next_branch = self.ops.len();
                self.ops[choice_idx] = Op::Choice(next_branch);
            }
        }
        let end_pc = self.ops.len();
        for idx in commit_jumps {
            self.ops[idx] = Op::Commit(end_pc);
        }
    }

    fn compile_repeat(&mut self, inner: &Pattern, min: usize, max: Option<usize>, greedy: bool) {
        if max.is_some_and(|m| m < min) {
            self.emit_fail();
            return;
        }
        // All unrolled/looped occurrences of `inner` belong to the same AST capture site(s).
        let base_site = self.next_site;
        let mut end_site = base_site;

        for _ in 0..min {
            self.next_site = base_site;
            self.compile(inner);
            end_site = end_site.max(self.next_site);
        }
        match max {
            Some(max_count) => {
                let extra = max_count.saturating_sub(min);
                let mut end_jumps = Vec::new();
                let mut greedy_choices = Vec::new();
                for _ in 0..extra {
                    let choice_idx = self.ops.len();
                    self.ops.push(Op::Choice(0));
                    if greedy {
                        greedy_choices.push(choice_idx);
                        self.next_site = base_site;
                        self.compile(inner);
                        end_site = end_site.max(self.next_site);
                    } else {
                        let skip_jump = self.ops.len();
                        self.ops.push(Op::Jump(0));
                        end_jumps.push(skip_jump);
                        let try_pc = self.ops.len();
                        self.ops[choice_idx] = Op::Choice(try_pc);
                        self.next_site = base_site;
                        self.compile(inner);
                        end_site = end_site.max(self.next_site);
                    }
                }
                let end_pc = self.ops.len();
                for idx in greedy_choices {
                    self.ops[idx] = Op::Choice(end_pc);
                }
                for idx in end_jumps {
                    self.ops[idx] = Op::Jump(end_pc);
                }
            }
            None => {
                if greedy {
                    let loop_start = self.ops.len();
                    self.ops.push(Op::Choice(0));
                    self.next_site = base_site;
                    self.compile(inner);
                    end_site = end_site.max(self.next_site);
                    self.ops.push(Op::Jump(loop_start));
                    let exit_pc = self.ops.len();
                    self.ops[loop_start] = Op::Choice(exit_pc);
                } else {
                    let loop_start = self.ops.len();
                    self.ops.push(Op::Choice(0));
                    let exit_jump = self.ops.len();
                    self.ops.push(Op::Jump(0));
                    let body_pc = self.ops.len();
                    self.ops[loop_start] = Op::Choice(body_pc);
                    self.next_site = base_site;
                    self.compile(inner);
                    end_site = end_site.max(self.next_site);
                    self.ops.push(Op::Jump(loop_start));
                    let exit_pc = self.ops.len();
                    self.ops[exit_jump] = Op::Jump(exit_pc);
                }
            }
        }
        self.next_site = end_site;
    }

    fn compile_sep_by(
        &mut self,
        item: &Pattern,
        sep: &Pattern,
        min: usize,
        max: Option<usize>,
        greedy: bool,
    ) {
        if max.is_some_and(|m| m < min) {
            self.emit_fail();
            return;
        }
        if min == 0 && max == Some(0) {
            return;
        }
        if min == 0 {
            let choice_idx = self.ops.len();
            self.ops.push(Op::Choice(0));
            if greedy {
                self.compile_sep_by_nonempty(item, sep, 1, max, true);
                let end_pc = self.ops.len();
                self.ops[choice_idx] = Op::Choice(end_pc);
            } else {
                let skip_jump = self.ops.len();
                self.ops.push(Op::Jump(0));
                let try_pc = self.ops.len();
                self.ops[choice_idx] = Op::Choice(try_pc);
                self.compile_sep_by_nonempty(item, sep, 1, max, false);
                let end_pc = self.ops.len();
                self.ops[skip_jump] = Op::Jump(end_pc);
            }
            return;
        }
        self.compile_sep_by_nonempty(item, sep, min, max, greedy);
    }

    fn compile_sep_by_nonempty(
        &mut self,
        item: &Pattern,
        sep: &Pattern,
        min: usize,
        max: Option<usize>,
        greedy: bool,
    ) {
        let item_base = self.next_site;
        self.compile(item);
        let item_end = self.next_site;

        let tail = Pattern::seq(vec![sep.clone(), item.clone()]);
        // Ensure the repeated `item` inside `tail` keeps the exact same site IDs as the first `item`.
        // Since `sep` precedes `item` inside `tail`, compile `sep` and `item` explicitly with fixed bases.
        let sep_base = item_end;
        let mut sep_end = sep_base;
        let extra_min = min - 1;
        for _ in 0..extra_min {
            self.next_site = sep_base;
            self.compile(sep);
            sep_end = sep_end.max(self.next_site);
            self.next_site = item_base;
            self.compile(item);
        }
        let _ = tail;
        match max {
            Some(max_count) => {
                let extra = max_count.saturating_sub(min);
                let mut end_jumps = Vec::new();
                let mut greedy_choices = Vec::new();
                for _ in 0..extra {
                    let choice_idx = self.ops.len();
                    self.ops.push(Op::Choice(0));
                    if greedy {
                        greedy_choices.push(choice_idx);
                    } else {
                        let skip_jump = self.ops.len();
                        self.ops.push(Op::Jump(0));
                        end_jumps.push(skip_jump);
                        let try_pc = self.ops.len();
                        self.ops[choice_idx] = Op::Choice(try_pc);
                    }
                    self.next_site = sep_base;
                    self.compile(sep);
                    sep_end = sep_end.max(self.next_site);
                    self.next_site = item_base;
                    self.compile(item);
                }
                let end_pc = self.ops.len();
                for idx in greedy_choices {
                    self.ops[idx] = Op::Choice(end_pc);
                }
                for idx in end_jumps {
                    self.ops[idx] = Op::Jump(end_pc);
                }
            }
            None => {
                if greedy {
                    let loop_start = self.ops.len();
                    self.ops.push(Op::Choice(0));
                    self.next_site = sep_base;
                    self.compile(sep);
                    sep_end = sep_end.max(self.next_site);
                    self.next_site = item_base;
                    self.compile(item);
                    self.ops.push(Op::Jump(loop_start));
                    let exit_pc = self.ops.len();
                    self.ops[loop_start] = Op::Choice(exit_pc);
                } else {
                    let loop_start = self.ops.len();
                    self.ops.push(Op::Choice(0));
                    let exit_jump = self.ops.len();
                    self.ops.push(Op::Jump(0));
                    let body_pc = self.ops.len();
                    self.ops[loop_start] = Op::Choice(body_pc);
                    self.next_site = sep_base;
                    self.compile(sep);
                    sep_end = sep_end.max(self.next_site);
                    self.next_site = item_base;
                    self.compile(item);
                    self.ops.push(Op::Jump(loop_start));
                    let exit_pc = self.ops.len();
                    self.ops[exit_jump] = Op::Jump(exit_pc);
                }
            }
        }
        self.next_site = sep_end.max(item_end);
    }
}
