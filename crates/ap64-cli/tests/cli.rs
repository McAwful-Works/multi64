//! `ap64-patch` as a user runs it.

use std::path::PathBuf;
use std::process::Command;

fn temp_dir(name: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("ap64-cli-{name}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    dir
}

/// `-o` naming the seed, however it is spelled, is refused before anything is read or written.
#[test]
fn an_output_that_is_the_seed_is_refused() {
    let dir = temp_dir("overwrite");
    let seed = dir.join("seed.z64");
    std::fs::write(&seed, b"the seed").unwrap();
    let mut outs = vec![seed.clone(), dir.join("sub").join("..").join("seed.z64")];
    if cfg!(windows) {
        outs.push(dir.join("SEED.Z64"));
    }
    for out in outs {
        let run = Command::new(env!("CARGO_BIN_EXE_ap64-patch"))
            .arg(&seed)
            .arg("-o")
            .arg(&out)
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&run.stderr);
        assert!(!run.status.success(), "{}", out.display());
        assert!(
            stderr.contains("would overwrite the seed"),
            "{}: {stderr}",
            out.display()
        );
        assert_eq!(std::fs::read(&seed).unwrap(), b"the seed");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// `-o` naming a hard link to the seed is refused too: it has a path of its own, but writing to
/// it writes over the seed.
#[test]
fn an_output_that_is_a_hard_link_to_the_seed_is_refused() {
    let dir = temp_dir("hard-link");
    let seed = dir.join("seed.z64");
    std::fs::write(&seed, b"the seed").unwrap();
    let link = dir.join("link.z64");
    std::fs::hard_link(&seed, &link).unwrap();
    let run = Command::new(env!("CARGO_BIN_EXE_ap64-patch"))
        .arg(&seed)
        .arg("-o")
        .arg(&link)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(!run.status.success(), "{stderr}");
    assert!(stderr.contains("would overwrite the seed"), "{stderr}");
    assert_eq!(std::fs::read(&seed).unwrap(), b"the seed");
    let _ = std::fs::remove_dir_all(&dir);
}
