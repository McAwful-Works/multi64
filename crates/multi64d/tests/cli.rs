//! The `multi64d` binary's own startup, run as a process. No cart: the serial device named here
//! does not exist, which the daemon starts without (`docs/spec/daemon-api-v1.md` §1.3.1).

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

const NO_SUCH_PORT: &str = "multi64d-test-no-such-serial-port";

/// A fresh, empty directory under the system temp dir, holding a `multi64d.toml` that does not
/// parse: a daemon that looks in its working directory exits on it at once.
fn dir_with_broken_config(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("multi64d-cli-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("multi64d.toml"), "this is = = not toml\n").unwrap();
    dir
}

/// The daemon in `cwd`, with no `MULTI64D_*` or `RUST_LOG` from the test's environment.
fn daemon(cwd: &Path, args: &[&str]) -> Child {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_multi64d"));
    for (name, _) in std::env::vars_os() {
        if name
            .to_string_lossy()
            .to_ascii_uppercase()
            .starts_with("MULTI64D_")
        {
            cmd.env_remove(name);
        }
    }
    cmd.env_remove("RUST_LOG")
        .env("NO_COLOR", "1")
        .current_dir(cwd)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn multi64d")
}

/// Every line the child writes to stdout and stderr, until both close.
fn lines(child: &mut Child) -> mpsc::Receiver<String> {
    let (tx, rx) = mpsc::channel();
    let out = child.stdout.take().unwrap();
    let err = child.stderr.take().unwrap();
    let tx2 = tx.clone();
    std::thread::spawn(move || {
        for l in BufReader::new(out).lines().map_while(Result::ok) {
            let _ = tx.send(l);
        }
    });
    std::thread::spawn(move || {
        for l in BufReader::new(err).lines().map_while(Result::ok) {
            let _ = tx2.send(l);
        }
    });
    rx
}

/// Wait up to `within` for a line containing `needle`; everything seen, and whether it was found.
fn wait_for(rx: &mpsc::Receiver<String>, needle: &str, within: Duration) -> (bool, Vec<String>) {
    let deadline = std::time::Instant::now() + within;
    let mut seen = Vec::new();
    while let Some(left) = deadline.checked_duration_since(std::time::Instant::now()) {
        match rx.recv_timeout(left) {
            Ok(l) => {
                let hit = l.contains(needle);
                seen.push(l);
                if hit {
                    return (true, seen);
                }
            }
            Err(_) => break,
        }
    }
    (false, seen)
}

const SERVE: [&str; 5] = [
    "--serial",
    NO_SUCH_PORT,
    "--listen",
    "127.0.0.1:0",
    "--no-print-ports",
];

#[test]
fn a_config_file_in_the_working_directory_is_loaded_without_no_config_file() {
    // The control for the test below: this directory's file does stop a daemon that looks.
    let dir = dir_with_broken_config("discovered");
    let mut child = daemon(&dir, &SERVE);
    let status = child.wait().unwrap();
    let out: Vec<String> = lines(&mut child).iter().collect();
    assert!(!status.success(), "{out:#?}");
    assert!(
        out.iter().any(|l| l.contains("multi64d.toml")),
        "the error names the file: {out:#?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn no_config_file_ignores_the_working_directory_and_starts() {
    // Multi64 starts its daemon this way, so a file meant for a hand-run daemon cannot reach it.
    let dir = dir_with_broken_config("ignored");
    let mut args = vec!["--no-config-file"];
    args.extend(SERVE);
    let mut child = daemon(&dir, &args);
    let rx = lines(&mut child);
    let (started, seen) = wait_for(&rx, "multi64d started", Duration::from_secs(10));
    let exited = child.try_wait().unwrap();
    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        started && exited.is_none(),
        "exited: {exited:?}; output: {seen:#?}"
    );
    assert!(
        !seen.iter().any(|l| l.contains("loaded config file")),
        "{seen:#?}"
    );
}

#[test]
fn no_config_file_refuses_an_explicit_config() {
    let dir = dir_with_broken_config("explicit");
    let file = dir.join("multi64d.toml");
    let mut args = vec!["--no-config-file", "--config", file.to_str().unwrap()];
    args.extend(SERVE);
    let mut child = daemon(&dir, &args);
    let status = child.wait().unwrap();
    let out: Vec<String> = lines(&mut child).iter().collect();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(!status.success(), "{out:#?}");
    assert!(
        out.iter()
            .any(|l| l.contains("--no-config-file") && l.contains("cannot be combined")),
        "the error names the conflict: {out:#?}"
    );
}
