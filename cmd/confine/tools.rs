//! The tools that `confine` can run for the command outside the sandbox,
//! each time you allow it: what each may be asked to do, and how each is
//! kept from running what the command wrote.
//!
//! A tool is code rather than configuration, as whether it is safe to run
//! outside the sandbox depends on what it does: `make`, `npm` and `cargo`
//! run what is in the directory, and so can never be tools.

mod gh;
mod git;

use std::{io, process::Command};

pub use git::Repo;

/// A tool that `confine` can run for the command.
pub trait Tool: Sync {
    /// The program it is, as the command runs it.
    fn name(&self) -> &'static str;

    /// What to do when the command runs it with `args`.
    fn policy(&self, args: &[&str]) -> Policy;

    /// Makes `command`, which runs it, safe to run outside the sandbox in
    /// `repo`, and returns what to do once it has run.
    fn prepare(&self, repo: &Repo, command: &mut Command) -> io::Result<Finish>;
}

/// What to do when the command runs a tool.
pub enum Policy {
    /// Never run it, for this reason.
    Never(String),
    /// Ask you every time.
    AskEachTime,
    /// Ask you, unless you allowed it always: what this names, such as
    /// `gh pr create`, which is what you allow.
    Ask(String),
}

/// What to do once a tool has run.
pub type Finish = Box<dyn FnOnce() -> io::Result<()> + Send>;

/// Every tool.
pub const TOOLS: &[&dyn Tool] = &[&gh::Gh];
