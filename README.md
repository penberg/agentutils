# agentutils

Small UNIX tools for letting a coding agent, or anything else you would
rather not trust with your files, work on a directory:

- **`fsfork`** forks a directory: it makes a copy-on-write clone of it, and
  prints where it is.
- **`confine`** runs a command in a fork of the working directory, or in a
  directory, that may write only there, and cannot read secrets.
- **`fsdiff`** shows what changed in a fork since it was made, as a patch.

Each does one thing. `confine` forks as `fsfork` does, and `fsdiff` knows
where a fork's base is; otherwise none knows about the others. They keep no
state:
a fork is a directory, its path is its name, and `ls`, `rm` and `git` do
the rest.

![fsfork forks a repository, rm -rf * in confine empties the fork, and
fsdiff shows the deletions, while the repository keeps its
files](.github/assets/demo.gif)

## Usage

Run Claude Code in a fork of the directory, where it may write nothing
else:

```sh
cd ~/src/turso
confine -w ~/.claude claude
```

`confine` forks the directory to `~/src/turso.1`, says so, and runs Claude
Code in the fork. `-w ~/.claude` lets it save its sessions and settings.
With `--rm`, the fork is removed when Claude Code exits; with `-d dir`,
there is no fork, and Claude Code runs in `dir` itself.

When it is done, see what it changed, apply the changes, and remove the
fork:

```sh
fsdiff ../turso.1
fsdiff ../turso.1 | git apply
rm -rf ../turso.1 ../.turso.1.base
```

Meanwhile, you can keep working in `~/src/turso`, and run more forks side by
side: `turso.2`, `turso.3`, and so on. `ls -d ../turso.*` lists them.

Each command has a man page: [`fsfork.1`](cmd/fsfork/fsfork.1),
[`confine.1`](cmd/confine/confine.1), and [`fsdiff.1`](cmd/fsdiff/fsdiff.1)
(`man ./cmd/fsfork/fsfork.1`).

## Install

```sh
make install
```

This builds the commands and installs them in `~/.local/bin`, and their man
pages in `~/.local/share/man`. To install them elsewhere, set `PREFIX`; in
`/usr/local`, that may need `make && sudo make install PREFIX=/usr/local`.
`make uninstall`, with the same `PREFIX`, removes them.

They run on macOS and Linux.

### Requirements

- macOS: APFS, with forks on the same volume as the directory.
- Linux: Linux 5.19 or later, with Landlock enabled, for `confine`. A file
  system that can share blocks between files, such as Btrfs or XFS, for
  `fsfork`; on others, a fork is a full copy.
- `git`, for `fsdiff`.

## fsfork

`fsfork` clones the directory twice, next to it: to the fork, `turso.1`,
and to its base, `.turso.1.base`, which keeps what the directory was when it
was forked. The fork is cloned from the base, so that the two are the same
whatever changes in the directory meanwhile. With no arguments, it forks the
working directory; given one, that directory; given two, to the second.

A fork is everything in the directory, as it is: the repository,
uncommitted changes, and build output, so builds start warm. On macOS, it is
made with `clonefile`, and on Linux with `cp --reflink`, so it takes no
space until either side is written to.

`fsfork` knows nothing about git: it forks exactly the directory it is
given.

## fsdiff

`fsdiff` compares a fork with its base, so it shows what changed in the
fork, and not what changed in the directory since. Given two directories, it
compares them instead. It prints a patch that `git apply` applies, and
exits as `diff` does: 0 if nothing changed, 1 if something did, and 2 if
something went wrong.

It leaves out `.git` directories, and what the base's `.gitignore` files
ignore, such as build output. New files in the fork are left out if the
fork's `.gitignore` files ignore them, but a file that was in the base never
is. Only what is in the patch reaches the directory it is applied to.

The fork is read but never trusted, as the command in it could have changed
anything there, its git configuration included, to make git run any
command. So `fsdiff` never runs git in the fork's repository: it records
both directories in a repository of its own, made in the temporary
directory and removed after, by git with no configuration but its defaults.
Symbolic links are shown as links and never followed, so a link to a file
outside the fork cannot bring that file into the patch.

## confine

`confine` runs a command in a fork of the working directory, or in the
directory given with `-d`, which the command may write to, along with
temporary directories, devices, build and package manager caches
(`~/.cargo`, `~/.rustup`, `~/.cache`, `~/.npm`, `~/Library/Caches`), and
paths given with `-w`. In the home directory, it can read only those, paths
given with `-r`, and what commands need to run: git's configuration,
`~/.claude.json`, toolchains such as `~/.local/bin` and `~/.nvm`, and on
macOS the keychains, whose items the keychain itself guards. So secrets,
such as `~/.ssh`, `~/.aws`, `~/.netrc`, or tokens exported in a shell's
startup files, are kept from the command without having to know where each
one is. Everything else, outside the home directory, is allowed. `confine`
then becomes the command, which exits with its own status; with `--rm`, it
waits for the command instead, removes the fork, and exits as the command
did.

It sets `CLAUDE_CODE_SANDBOXED=1`, so that Claude Code trusts the directory
without asking, which would write to `~/.claude.json`.

How the command is confined depends on the platform.

### macOS

The command runs under Seatbelt, the macOS sandbox, through `sandbox-exec`.
`confine` writes a profile that allows everything, then denies writing
everywhere but the writable paths, and denies listing and reading the home
directory but the readable and writable paths. Since later rules win, the
directory, which may lie in the home directory, is allowed last. The profile gives the paths as
parameters rather than in the text, and Seatbelt matches them after
resolving symbolic links, so they cannot be escaped by quoting or linking.

`sandbox-exec` cannot run inside another sandbox, so a confined command
cannot confine another.

### Linux

The command runs under Landlock, which a process uses to confine itself and
the processes it starts, without privileges. `confine` makes a ruleset in
which reading and running files, and changing anything, are denied unless
granted, and applies it just before becoming the command. Landlock needs
Linux 5.19 or later; without it, `confine` fails rather than run the command
unconfined.

Landlock can only grant rights, to a file and everything under it, and
cannot take them away from something inside. So rather than denying the
home directory, `confine` grants reading to what is around it: it lists
each directory above it, and grants reading to every entry in it but the
one the home directory is in, and so on down. Then it grants reading to
the readable paths, reading and writing to the writable ones, and both to
the directory itself. Because of this, on Linux:

- The command can list the files in any directory, such as the names of
  the keys in `~/.ssh`, but cannot read them.
- Files made in a directory above the home directory after the command
  starts, such as a new home directory in `/home`, cannot be read.
- The command cannot run set-user-ID programs such as `sudo`, as Landlock
  confines a process without privileges only if it can gain none.

Unlike `sandbox-exec`, Landlock works inside another Landlock sandbox, so a
confined command can confine another.

### What is not confined

The network: a command can send anything it can read to anywhere, and can
push to remotes whose credentials it can reach, such as through the macOS
keychain or an SSH agent.

Commits made in a fork are not signed if signing needs `~/.gnupg` or
`~/.ssh`, which the command cannot read.

What the command may write to, it may read, so secrets kept in a writable
path can be read, such as a registry token in `~/.cargo/credentials.toml`.

## License

MIT
