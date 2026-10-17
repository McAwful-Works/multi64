fn main() {
    let manifest_dir = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let workspace_root = manifest_dir.join("..").join("..").join("..");
    let profile = std::env::var("PROFILE").unwrap_or_else(|_| "debug".into());
    let resources_dir = manifest_dir.join("resources");
    let _ = std::fs::create_dir_all(&resources_dir);

    // Bundle the daemon when it exists (run `cargo build -p multi64d` first). The binary carries the
    // host's extension (`multi64d` on Unix, `multi64d.exe` on Windows) but `bundle.resources` in
    // tauri.conf.json always names `multi64d.exe`, so copy to that fixed name. Without this file
    // tauri-build fails outright — a missing declared resource is a hard error, not a warning.
    let daemon_name = format!("multi64d{}", std::env::consts::EXE_SUFFIX);
    let daemon = workspace_root
        .join("target")
        .join(&profile)
        .join(&daemon_name);
    let dest_daemon = resources_dir.join("multi64d.exe");

    // `tauri_build::build()` below prints `rerun-if-changed` lines of its own, and any such line
    // replaces Cargo's default of rerunning on every change in the package. Without this one, a
    // rebuilt daemon was never copied again and the app went on bundling the old one.
    println!("cargo:rerun-if-changed={}", daemon.display());

    if daemon.is_file() {
        // A copy that fails would leave whatever `resources/` already held, bundled as if current.
        if let Err(e) = std::fs::copy(&daemon, &dest_daemon) {
            panic!(
                "could not copy {} to {}: {e}",
                daemon.display(),
                dest_daemon.display()
            );
        }
        println!(
            "cargo:warning=bundled {daemon_name} from {}",
            daemon.display()
        );
    } else if dest_daemon.is_file() {
        println!(
            "cargo:warning={daemon_name} not found at {}, so {} is left from an earlier build and may be stale — run `cargo build -p multi64d` with this profile first",
            daemon.display(),
            dest_daemon.display()
        );
    } else {
        println!(
            "cargo:warning={daemon_name} not found at {} — run `cargo build -p multi64d` with this profile first",
            daemon.display()
        );
    }

    tauri_build::build();
}
