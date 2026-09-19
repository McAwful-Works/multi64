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
    if daemon.is_file() {
        let _ = std::fs::copy(&daemon, &dest_daemon);
        println!(
            "cargo:warning=bundled {daemon_name} from {}",
            daemon.display()
        );
    } else {
        println!(
            "cargo:warning={daemon_name} not found at {} — run `cargo build -p multi64d` with this profile first",
            daemon.display()
        );
    }

    tauri_build::build();
}
