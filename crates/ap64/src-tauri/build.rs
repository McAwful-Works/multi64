use std::process::Command;

/// `git` in the repository, trimmed, if it answers.
fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn main() {
    // The commit this was built from, for the first line of every session log: a log from
    // someone else's machine says which code wrote it. "unknown" outside a git checkout.
    let commit = git(&["rev-parse", "--short", "HEAD"]).unwrap_or_else(|| "unknown".into());
    println!("cargo:rustc-env=AP64_COMMIT={commit}");
    // Built again when HEAD moves: a checkout, or a commit on the current branch. In a worktree
    // HEAD is in the worktree's git dir and the branches in the common one.
    for dir in ["--git-dir", "--git-common-dir"] {
        if let Some(d) = git(&["rev-parse", dir]) {
            // Only paths that exist: Cargo takes a missing one as changed on every build.
            for f in ["HEAD", "refs/heads", "packed-refs"] {
                let path = std::path::Path::new(&d).join(f);
                if path.exists() {
                    println!("cargo:rerun-if-changed={}", path.display());
                }
            }
        }
    }
    tauri_build::build()
}
