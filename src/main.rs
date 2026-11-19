//! Detect and remove AI coding agent markers from git commits.
//!
//! `pre-commit-deagent fix` (post-commit hook): strips agent trailers and session links
//! from HEAD's message, resets author/committer to the configured git identity and signs
//! the commit, by rewriting HEAD with plumbing (no index, no hooks).
//!
//! `pre-commit-deagent check [<rev-range>]` (pre-push / CI): reports offending commits
//! and exits non-zero.

mod markers;

use std::io::Write;
use std::process::{Command, Stdio, exit};

use markers::{is_agent_ident, strip_markers};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("fix") | None => fix(),
        Some("check") => check(args.get(1).map(String::as_str)),
        Some(other) => Err(format!(
            "unknown command `{other}`; usage: pre-commit-deagent [fix | check [<rev-range>]]"
        )),
    };
    match result {
        Ok(true) => {}
        Ok(false) => exit(1),
        Err(e) => {
            eprintln!("deagent: error: {e}");
            exit(2);
        }
    }
}

/// A git command whose config and identity cannot be overridden from the environment,
/// e.g. by an agent running `git -c user.name=... commit` or `GIT_AUTHOR_NAME=... git commit`.
fn git() -> Command {
    let mut cmd = Command::new("git");
    for (key, _) in std::env::vars_os() {
        let k = key.to_string_lossy();
        if k == "GIT_CONFIG_PARAMETERS"
            || k == "GIT_CONFIG_COUNT"
            || k.starts_with("GIT_CONFIG_KEY_")
            || k.starts_with("GIT_CONFIG_VALUE_")
            || k.starts_with("GIT_AUTHOR_")
            || k.starts_with("GIT_COMMITTER_")
        {
            cmd.env_remove(&key);
        }
    }
    cmd
}

fn run(cmd: &mut Command) -> Result<String, String> {
    let out = cmd.output().map_err(|e| format!("running git: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "{:?} failed: {}",
            cmd.get_args().collect::<Vec<_>>(),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    String::from_utf8(out.stdout).map_err(|_| "git output is not UTF-8".into())
}

fn config(key: &str) -> Option<String> {
    run(git().args(["config", "--get", key]))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

struct Commit {
    tree: String,
    parents: Vec<String>,
    /// `Name <email>`
    author: String,
    /// `<unix-seconds> <tz>`
    author_date: String,
    committer: String,
    signed: bool,
    message: String,
}

fn read_commit(rev: &str) -> Result<Commit, String> {
    let raw = run(git().args(["cat-file", "commit", rev]))?;
    let (headers, message) = raw.split_once("\n\n").unwrap_or((&raw, ""));
    let mut c = Commit {
        tree: String::new(),
        parents: vec![],
        author: String::new(),
        author_date: String::new(),
        committer: String::new(),
        signed: false,
        message: message.to_string(),
    };
    for line in headers.lines() {
        let (key, value) = line.split_once(' ').unwrap_or((line, ""));
        match key {
            "tree" => c.tree = value.into(),
            "parent" => c.parents.push(value.into()),
            "author" => (c.author, c.author_date) = split_ident(value),
            "committer" => c.committer = split_ident(value).0,
            "gpgsig" | "gpgsig-sha256" => c.signed = true,
            "encoding" if !value.eq_ignore_ascii_case("utf-8") => {
                return Err(format!("commit uses encoding {value}, not touching it"));
            }
            _ => {}
        }
    }
    Ok(c)
}

/// Split `Name <email> 1700000000 +0100` into (`Name <email>`, `1700000000 +0100`).
fn split_ident(value: &str) -> (String, String) {
    match value.rfind('>') {
        Some(i) => (value[..=i].into(), value[i + 1..].trim().into()),
        None => (value.into(), String::new()),
    }
}

fn fix() -> Result<bool, String> {
    let git_dir = run(git().args(["rev-parse", "--git-dir"]))?;
    let git_dir = std::path::Path::new(git_dir.trim());
    let head = run(git().args(["rev-parse", "HEAD"]))?.trim().to_string();
    let commit = read_commit(&head)?;

    let (message, removed) = strip_markers(&commit.message);
    let identity = match (config("user.name"), config("user.email")) {
        (Some(name), Some(email)) => {
            Some((name.clone(), email.clone(), format!("{name} <{email}>")))
        }
        _ => None,
    };
    let sign = config("user.signingkey").is_some()
        || config("commit.gpgsign").is_some_and(|v| v == "true");

    let mut reasons = vec![];
    if !removed.is_empty() {
        reasons.push(format!("stripped {} marker line(s)", removed.len()));
    }
    if let Some((_, _, ident)) = &identity {
        if &commit.author != ident {
            reasons.push(format!("author {} -> {ident}", commit.author));
        }
        if &commit.committer != ident {
            reasons.push(format!("committer {} -> {ident}", commit.committer));
        }
    }
    if sign && !commit.signed {
        reasons.push("signed".into());
    }
    if reasons.is_empty() {
        return Ok(true);
    }
    for line in &removed {
        eprintln!("deagent: - {line}");
    }

    if std::env::var_os("CI").is_some() {
        eprintln!(
            "deagent: CI detected, not rewriting HEAD ({})",
            reasons.join(", ")
        );
        return Ok(false);
    }
    // shortcut: rewriting HEAD mid-rebase/cherry-pick would fight the sequencer; use `check` there.
    if ["rebase-merge", "rebase-apply", "sequencer"]
        .iter()
        .any(|d| git_dir.join(d).exists())
    {
        eprintln!(
            "deagent: rebase/cherry-pick in progress, not rewriting HEAD ({})",
            reasons.join(", ")
        );
        return Ok(false);
    }
    if message.trim().is_empty() {
        eprintln!("deagent: message would be empty after stripping, not rewriting HEAD");
        return Ok(false);
    }

    let mut cmd = git();
    cmd.arg("commit-tree").arg(&commit.tree);
    for p in &commit.parents {
        cmd.args(["-p", p]);
    }
    if sign {
        cmd.arg("-S");
    }
    match &identity {
        Some((name, email, _)) => {
            cmd.env("GIT_AUTHOR_NAME", name)
                .env("GIT_AUTHOR_EMAIL", email)
                .env("GIT_COMMITTER_NAME", name)
                .env("GIT_COMMITTER_EMAIL", email);
        }
        None => {
            let (name, email) = commit
                .author
                .split_once(" <")
                .unwrap_or((&commit.author, ""));
            cmd.env("GIT_AUTHOR_NAME", name)
                .env("GIT_AUTHOR_EMAIL", email.trim_end_matches('>'));
        }
    }
    cmd.env("GIT_AUTHOR_DATE", format!("@{}", commit.author_date));
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("running git commit-tree: {e}"))?;
    child
        .stdin
        .take()
        .expect("stdin is piped")
        .write_all(message.as_bytes())
        .map_err(|e| format!("writing commit message: {e}"))?;
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(format!(
            "git commit-tree failed, HEAD left untouched: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let new = String::from_utf8_lossy(&out.stdout).trim().to_string();
    run(git().args([
        "update-ref",
        "-m",
        "deagent: rewrite agent commit",
        "HEAD",
        &new,
        &head,
    ]))?;
    eprintln!(
        "deagent: rewrote HEAD {:.10} -> {:.10} ({})",
        head,
        new,
        reasons.join(", ")
    );
    Ok(true)
}

fn check(range: Option<&str>) -> Result<bool, String> {
    let from = std::env::var("PRE_COMMIT_FROM_REF")
        .ok()
        .filter(|s| !s.is_empty());
    let to = std::env::var("PRE_COMMIT_TO_REF")
        .ok()
        .filter(|s| !s.is_empty());
    let mut cmd = git();
    cmd.arg("rev-list");
    match (range, from, to) {
        (Some(r), _, _) => cmd.arg(r),
        (None, Some(f), Some(t)) if f.trim_matches('0').is_empty() => {
            cmd.args([t.as_str(), "--not", "--remotes"])
        }
        (None, Some(f), Some(t)) => cmd.arg(format!("{f}..{t}")),
        _ => cmd.args(["-1", "HEAD"]),
    };
    let mut ok = true;
    for rev in run(&mut cmd)?.lines() {
        let c = read_commit(rev)?;
        let mut problems: Vec<String> = strip_markers(&c.message).1;
        for (role, ident) in [("author", &c.author), ("committer", &c.committer)] {
            if is_agent_ident(ident) {
                problems.push(format!("{role} is an agent: {ident}"));
            }
        }
        if !problems.is_empty() {
            ok = false;
            eprintln!(
                "deagent: {rev:.10} {}",
                c.message.lines().next().unwrap_or("")
            );
            for p in problems {
                eprintln!("deagent:   {p}");
            }
        }
    }
    Ok(ok)
}
