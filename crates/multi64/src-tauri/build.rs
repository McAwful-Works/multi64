fn main() {
    println!("cargo:rerun-if-env-changed=MULTI64_XFER64_BUNDLE");

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
        println!("cargo:warning=bundled {daemon_name} from {}", daemon.display());
    } else {
        println!(
            "cargo:warning={daemon_name} not found at {} — run `cargo build -p multi64d` with this profile first",
            daemon.display()
        );
    }

    // Xfer64 installer: pair NSIS Multi64 ↔ Xfer64 NSIS, or MSI Multi64 ↔ Xfer64 MSI.
    // Set MULTI64_XFER64_BUNDLE=msi when building the Multi64 MSI so resources/xfer64-setup.msi is populated.
    // Default (unset or "nsis") uses the Xfer64 *-setup.exe from bundle/nsis/.
    let xfer_kind = std::env::var("MULTI64_XFER64_BUNDLE")
        .unwrap_or_else(|_| "nsis".into())
        .to_ascii_lowercase();
    let xfer_msi = xfer_kind == "msi";

    let bundle_root = workspace_root.join("target").join(&profile).join("bundle");
    let nsis_dir = bundle_root.join("nsis");
    let msi_dir = bundle_root.join("msi");
    let dest_exe = resources_dir.join("xfer64-setup.exe");
    let dest_msi = resources_dir.join("xfer64-setup.msi");

    fn xfer64_bundle_name_matches_lower(n: &str) -> bool {
        n.contains("xfer64") || n.contains("multi64-cart-explorer")
    }

    fn xfer64_nsis_matches(name: &str) -> bool {
        let n = name.to_ascii_lowercase();
        n.ends_with("-setup.exe") && xfer64_bundle_name_matches_lower(&n)
    }

    fn xfer64_msi_matches(name: &str) -> bool {
        let n = name.to_ascii_lowercase();
        n.ends_with(".msi") && xfer64_bundle_name_matches_lower(&n)
    }

    let mut found = false;
    if xfer_msi {
        if let Ok(rd) = std::fs::read_dir(&msi_dir) {
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().to_string();
                if xfer64_msi_matches(&name) {
                    let _ = std::fs::copy(e.path(), &dest_msi);
                    println!("cargo:rerun-if-changed={}", e.path().display());
                    println!(
                        "cargo:warning=bundled xfer64-setup.msi from {}",
                        e.path().display()
                    );
                    if let Ok(meta) = std::fs::metadata(&dest_msi) {
                        println!(
                            "cargo:warning=xfer64-setup.msi → {} bytes (Multi64 MSI ↔ Xfer64 MSI)",
                            meta.len()
                        );
                    }
                    found = true;
                    break;
                }
            }
        }
        let _ = std::fs::write(&dest_exe, b"");
        if !found {
            let _ = std::fs::write(&dest_msi, b"");
            println!(
                "cargo:warning=Xfer64 MSI not found under {} — placeholder xfer64-setup.msi; build xfer64 with `npm run build` (or `tauri build --bundles msi`) before packaging, and set MULTI64_XFER64_BUNDLE=msi",
                msi_dir.display()
            );
        }
    } else {
        if let Ok(rd) = std::fs::read_dir(&nsis_dir) {
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().to_string();
                if xfer64_nsis_matches(&name) {
                    let _ = std::fs::copy(e.path(), &dest_exe);
                    println!("cargo:rerun-if-changed={}", e.path().display());
                    println!(
                        "cargo:warning=bundled xfer64-setup.exe from {}",
                        e.path().display()
                    );
                    if let Ok(meta) = std::fs::metadata(&dest_exe) {
                        println!(
                            "cargo:warning=xfer64-setup.exe → {} bytes (Multi64 NSIS ↔ Xfer64 NSIS)",
                            meta.len()
                        );
                    }
                    found = true;
                    break;
                }
            }
        }
        let _ = std::fs::write(&dest_msi, b"");
        if !found && !dest_exe.is_file() {
            let _ = std::fs::write(&dest_exe, b"");
            println!(
                "cargo:warning=Xfer64 NSIS installer not found — placeholder xfer64-setup.exe; run npm run build in crates/xfer64 before release packaging"
            );
        } else if !found
            && std::fs::metadata(&dest_exe)
                .map(|m| m.len() == 0)
                .unwrap_or(false)
        {
            println!(
                "cargo:warning=xfer64-setup.exe is still 0 bytes — UI will show Install disabled until you build Xfer64 and rebuild multi64"
            );
        }
    }

    println!("cargo:rerun-if-changed={}", dest_exe.display());
    println!("cargo:rerun-if-changed={}", dest_msi.display());

    tauri_build::build();
}
