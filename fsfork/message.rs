//! What the commands say, on stderr, all in one form: `prog: subject:
//! problem`, on one line, in lowercase, with the subject a path as it would
//! be typed from the working directory.

use std::{
    env, io,
    path::{Path, PathBuf},
    process::ExitCode,
};

use argh::FromArgs;

/// The name the command was run as.
pub fn prog() -> String {
    env::args_os()
        .next()
        .as_deref()
        .and_then(|arg0| Path::new(arg0).file_name())
        .map_or_else(
            || "fsfork".into(),
            |name| name.to_string_lossy().into_owned(),
        )
}

/// Says `message` on stderr, after the command's name.
pub fn say(message: impl std::fmt::Display) {
    eprintln!("{}: {message}", prog());
}

/// What went wrong in `e`: for an error from the system, its description,
/// in lowercase and without its number, as in "no such file or directory".
pub fn describe(e: &io::Error) -> String {
    let text = e.to_string();
    let Some(code) = e.raw_os_error() else {
        return text;
    };
    let text = text
        .strip_suffix(&format!(" (os error {code})"))
        .unwrap_or(&text);
    lowercase_first(text)
}

/// `e`, saying it is about `path`.
pub fn at(path: &Path, e: io::Error) -> io::Error {
    io::Error::new(
        e.kind(),
        format!("{}: {}", shown(path).display(), describe(&e)),
    )
}

/// `path` as it would be typed from the working directory: relative to it if
/// it is in it or next to it, as forks are, and otherwise as it is.
pub fn shown(path: &Path) -> PathBuf {
    if path.is_relative() {
        return path.to_path_buf();
    }
    let Ok(cwd) = env::current_dir().and_then(|cwd| cwd.canonicalize()) else {
        return path.to_path_buf();
    };
    if let Ok(inside) = path.strip_prefix(&cwd) {
        return if inside.as_os_str().is_empty() {
            PathBuf::from(".")
        } else {
            inside.to_path_buf()
        };
    }
    if let Some(parent) = cwd.parent().filter(|parent| parent.parent().is_some())
        && let Ok(next) = path.strip_prefix(parent)
    {
        return Path::new("..").join(next);
    }
    path.to_path_buf()
}

/// The command's arguments, or, if they are not right, the code to exit
/// with, having said why and how to give them, as in `usage`, its synopsis
/// with a line for each form.
pub fn parse<T: FromArgs>(usage: &str, failure: u8) -> Result<T, ExitCode> {
    let args: Vec<String> = env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let prog = prog();
    T::from_args(&[&prog], &args).map_err(|exit| match exit.status {
        Ok(()) => {
            print!("{}", exit.output);
            ExitCode::SUCCESS
        }
        Err(()) => {
            let problem = exit.output.lines().next().unwrap_or_default();
            usage_error(problem.trim_end_matches(['.', ':']), usage, failure)
        }
    })
}

/// Says `problem` with how to give the arguments, as in `usage`, and returns
/// the code to exit with, `failure`.
pub fn usage_error(problem: &str, usage: &str, failure: u8) -> ExitCode {
    say(lowercase_first(problem));
    for (i, form) in usage.lines().enumerate() {
        let label = if i == 0 { "usage:" } else { "      " };
        eprintln!("{label} {form}");
    }
    ExitCode::from(failure)
}

fn lowercase_first(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_lowercase().chain(chars).collect(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describes_system_errors_in_lowercase_without_their_number() {
        let e = io::Error::from_raw_os_error(libc::ENOENT);
        assert_eq!(describe(&e), "no such file or directory");
        let e = io::Error::other("Mine as it is");
        assert_eq!(describe(&e), "Mine as it is");
    }

    #[test]
    fn shows_paths_from_the_working_directory() {
        let cwd = env::current_dir().unwrap().canonicalize().unwrap();
        let parent = cwd.parent().unwrap();
        assert_eq!(shown(Path::new("as/typed")), Path::new("as/typed"));
        assert_eq!(shown(&cwd), Path::new("."));
        assert_eq!(shown(&cwd.join("a")), Path::new("a"));
        assert_eq!(shown(&parent.join("x.1")), Path::new("../x.1"));
        assert_eq!(shown(Path::new("/")), Path::new("/"));
    }
}
