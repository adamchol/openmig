//! The few git queries `generate --base` and `check --base` need.

use openmig_core::Error;
use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;

fn git(dir: &Path, args: &[&str]) -> Result<String, Error> {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .map_err(|e| Error::io("running git", e))?;
    if !out.status.success() {
        return Err(Error::other(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The path of `dir` relative to the repository root, with a trailing `/`.
fn relative_dir(dir: &Path) -> Result<String, Error> {
    let prefix = git(dir, &["rev-parse", "--show-prefix"])?;
    Ok(prefix.trim().to_owned())
}

/// File names directly inside `dir` at `git_ref`.
pub fn files_in_ref(dir: &Path, git_ref: &str) -> Result<BTreeSet<String>, Error> {
    let rel = relative_dir(dir)?;
    let listing = git(
        dir,
        &[
            "ls-tree",
            "--full-tree",
            "--name-only",
            &format!("{git_ref}:{rel}"),
        ],
    );
    match listing {
        Ok(out) => Ok(out.lines().map(str::to_owned).collect()),
        Err(_) => {
            git(
                dir,
                &["rev-parse", "--verify", &format!("{git_ref}^{{commit}}")],
            )?;
            Ok(BTreeSet::new())
        }
    }
}

/// Content of `dir/<file>` at `git_ref`.
pub fn file_at_ref(dir: &Path, git_ref: &str, file: &str) -> Result<String, Error> {
    let rel = relative_dir(dir)?;
    git(dir, &["show", &format!("{git_ref}:{rel}{file}")])
}
