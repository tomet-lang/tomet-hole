//! Bytecode compiler from [`Pattern`] IR to a flat [`Program`].

use crate::pattern::{BuiltinMatcher, CharClass, Pattern};

/// A single bytecode instruction for the `hole-engine` VM.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Op {
    Literal(String),
    AnyChar { cross_newline: bool },
    Class(CharClass),
    Builtin(BuiltinMatcher),
    Balanced { open: String, close: String },
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
    /// Begin a capture group at the current byte position.
    OpenCapture(Option<String>),
    /// End the most recently opened capture group.
    CloseCapture,
    /// Begin a negative lookahead (`Not`): if body succeeds, jump to `fail_pc` semantics via `NotCommit`.
    NotStart(usize),
    /// Succeeds negative lookahead body -> fail the outer `Not`.
    NotFail,
    /// Begin a positive lookahead: saves position, body runs, then restores position on `LookaheadEnd`.
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
        let mut ops = Vec::new();
        compile_into(pattern, &mut ops);
        ops.push(Op::Match);
        Self { ops }
    }

    /// Returns the compiled bytecode instructions.
    pub fn ops(&self) -> &[Op] {
        &self.ops
    }
}

fn compile_into(pattern: &Pattern, ops: &mut Vec<Op>) {
    match pattern {
        Pattern::Literal(s) => {
            if !s.is_empty() {
                ops.push(Op::Literal(s.clone()));
            }
        }
        Pattern::AnyChar { cross_newline } => {
            ops.push(Op::AnyChar {
                cross_newline: *cross_newline,
            });
        }
        Pattern::Class(class) => {
            ops.push(Op::Class(class.clone()));
        }
        Pattern::Builtin(b) => {
            ops.push(Op::Builtin(b.clone()));
        }
        Pattern::Balanced { open, close } => {
            ops.push(Op::Balanced {
                open: open.clone(),
                close: close.clone(),
            });
        }
        Pattern::Start => ops.push(Op::StartAnchor),
        Pattern::End => ops.push(Op::EndAnchor),
        Pattern::WordBoundary => ops.push(Op::WordBoundary),
        Pattern::Seq(parts) => {
            for part in parts {
                compile_into(part, ops);
            }
        }
        Pattern::Capture { name, inner } => {
            ops.push(Op::OpenCapture(name.clone()));
            compile_into(inner, ops);
            ops.push(Op::CloseCapture);
        }
        Pattern::Alt(branches) => {
            if branches.is_empty() {
                // Always fails: a `NotStart` that jumps to a succeeding `NotFail`.
                let after = ops.len() + 2;
                ops.push(Op::NotStart(after));
                ops.push(Op::NotFail);
                return;
            }
            if branches.len() == 1 {
                compile_into(&branches[0], ops);
                return;
            }
            let mut commit_jumps = Vec::with_capacity(branches.len() - 1);
            for (i, branch) in branches.iter().enumerate() {
                let is_last = i + 1 == branches.len();
                if is_last {
                    compile_into(branch, ops);
                } else {
                    let choice_idx = ops.len();
                    ops.push(Op::Choice(0));
                    compile_into(branch, ops);
                    let commit_idx = ops.len();
                    ops.push(Op::Commit(0));
                    commit_jumps.push(commit_idx);
                    let next_branch = ops.len();
                    ops[choice_idx] = Op::Choice(next_branch);
                }
            }
            let end_pc = ops.len();
            for idx in commit_jumps {
                ops[idx] = Op::Commit(end_pc);
            }
        }
        Pattern::Repeat {
            inner,
            min,
            max,
            greedy,
        } => {
            for _ in 0..*min {
                compile_into(inner, ops);
            }
            match max {
                Some(max_count) => {
                    let extra = max_count.saturating_sub(*min);
                    let mut end_jumps = Vec::new();
                    let mut greedy_choices = Vec::new();
                    for _ in 0..extra {
                        let choice_idx = ops.len();
                        ops.push(Op::Choice(0));
                        if *greedy {
                            greedy_choices.push(choice_idx);
                            compile_into(inner, ops);
                        } else {
                            // Lazy: try skipping first, on backtrack try one more.
                            let skip_jump = ops.len();
                            ops.push(Op::Jump(0));
                            end_jumps.push(skip_jump);
                            let try_pc = ops.len();
                            ops[choice_idx] = Op::Choice(try_pc);
                            compile_into(inner, ops);
                        }
                    }
                    let end_pc = ops.len();
                    for idx in greedy_choices {
                        ops[idx] = Op::Choice(end_pc);
                    }
                    for idx in end_jumps {
                        ops[idx] = Op::Jump(end_pc);
                    }
                }
                None => {
                    if *greedy {
                        // Greedy loop: Choice(exit) -> inner -> Jump(loop_start)
                        let loop_start = ops.len();
                        ops.push(Op::Choice(0));
                        compile_into(inner, ops);
                        ops.push(Op::Jump(loop_start));
                        let exit_pc = ops.len();
                        ops[loop_start] = Op::Choice(exit_pc);
                    } else {
                        // Lazy loop: Choice(body) -> Jump(exit); body: inner -> Jump(loop_start)
                        let loop_start = ops.len();
                        ops.push(Op::Choice(0));
                        let exit_jump = ops.len();
                        ops.push(Op::Jump(0));
                        let body_pc = ops.len();
                        ops[loop_start] = Op::Choice(body_pc);
                        compile_into(inner, ops);
                        ops.push(Op::Jump(loop_start));
                        let exit_pc = ops.len();
                        ops[exit_jump] = Op::Jump(exit_pc);
                    }
                }
            }
        }
        Pattern::Not(inner) => {
            let not_idx = ops.len();
            ops.push(Op::NotStart(0));
            compile_into(inner, ops);
            ops.push(Op::NotFail);
            let after_pc = ops.len();
            ops[not_idx] = Op::NotStart(after_pc);
        }
        Pattern::Lookahead(inner) => {
            let la_idx = ops.len();
            ops.push(Op::LookaheadStart(0));
            compile_into(inner, ops);
            ops.push(Op::LookaheadEnd);
            let after_pc = ops.len();
            ops[la_idx] = Op::LookaheadStart(after_pc);
        }
    }
}
