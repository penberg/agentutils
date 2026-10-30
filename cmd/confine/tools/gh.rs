//! `gh`, GitHub's command, so that the command can open pull requests and
//! the like though it cannot read `gh`'s configuration.

use std::{io, process::Command};

use super::{Finish, Policy, Repo, Tool};

/// `gh`'s commands that manage credentials or configuration, or run
/// extensions, which are never run.
const NEVER: &[&str] = &[
    "alias",
    "auth",
    "config",
    "ext",
    "extension",
    "extensions",
    "gpg-key",
    "secret",
    "ssh-key",
    "variable",
];

/// `gh`'s commands that can do anything, which you are asked about every
/// time.
const UNBOUNDED: &[&str] = &["api"];

pub struct Gh;

impl Tool for Gh {
    fn name(&self) -> &'static str {
        "gh"
    }

    fn policy(&self, args: &[&str]) -> Policy {
        // The command, such as `gh pr create`: the words before the first
        // flag, up to two.
        let words: Vec<&str> = args
            .iter()
            .take_while(|arg| !arg.starts_with('-'))
            .take(2)
            .copied()
            .collect();
        match words.first() {
            Some(word) if NEVER.contains(word) => Policy::Never(format!("gh {word}")),
            Some(word) if !UNBOUNDED.contains(word) => {
                Policy::Ask(format!("gh {}", words.join(" ")))
            }
            _ => Policy::AskEachTime,
        }
    }

    /// `gh` runs git.
    fn prepare(&self, repo: &Repo, command: &mut Command) -> io::Result<Finish> {
        repo.isolate(command)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(args: &[&str]) -> String {
        match Gh.policy(args) {
            Policy::Never(why) => format!("never: {why}"),
            Policy::AskEachTime => "each time".to_string(),
            Policy::Ask(what) => what,
        }
    }

    #[test]
    fn allows_always_what_the_command_names() {
        assert_eq!(policy(&["pr", "create", "--title", "x"]), "gh pr create");
        assert_eq!(policy(&["status"]), "gh status");
        assert_eq!(policy(&["auth", "token"]), "never: gh auth");
        assert_eq!(policy(&["api", "/user"]), "each time");
        assert_eq!(policy(&["--version"]), "each time");
    }
}
