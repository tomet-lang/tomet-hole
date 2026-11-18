//! Pure, Tomet-independent pattern matching engine.
//!
//! Provides the core pattern representation ([`Pattern`]), bytecode compiler
//! ([`Program`]), and backtracking VM ([`Match`], [`Capture`]) without
//! depending on Tomet AST or syntax crates.

pub mod pattern;
pub mod program;
pub mod template;
pub mod vm;

pub use pattern::{BuiltinMatcher, CaseStyle, CharClass, DEFAULT_NANOID_LENGTH, Pattern};
pub use program::{Op, Program};
pub use template::{Template, TemplateError};
pub use vm::{Capture, Match};
