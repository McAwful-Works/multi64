// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() >= 2 && args[1] == "upload" {
        match xfer64_lib::parse_upload_args() {
            Ok(a) if a.picker => {
                if a.paths.is_empty() {
                    eprintln!("xfer64 upload: no files (picker mode needs at least one file)");
                    #[cfg(windows)]
                    {
                        let _ = rfd::MessageDialog::new()
                            .set_title("Xfer64 upload")
                            .set_description(
                                "No files to upload.\n\nUse Send to or pick files first.",
                            )
                            .set_level(rfd::MessageLevel::Error)
                            .show();
                    }
                    std::process::exit(1);
                }
                std::env::set_var(
                    "MULTI64_XFER64_UPLOAD_PICKER_PATHS",
                    serde_json::to_string(&a.paths).expect("serialize upload paths"),
                );
                xfer64_lib::run();
                return;
            }
            Ok(a) => match xfer64_lib::run_cli_upload_from_args(a) {
                Ok(()) => std::process::exit(0),
                Err(e) => {
                    eprintln!("xfer64 upload: {e}");
                    #[cfg(windows)]
                    {
                        let _ = rfd::MessageDialog::new()
                            .set_title("Xfer64 upload")
                            .set_description(format!("xfer64 upload:\n{e}"))
                            .set_level(rfd::MessageLevel::Error)
                            .show();
                    }
                    std::process::exit(1);
                }
            },
            Err(e) => {
                if e.contains("usage:") {
                    eprintln!("{e}");
                    std::process::exit(0);
                }
                eprintln!("xfer64 upload: {e}");
                #[cfg(windows)]
                {
                    let _ = rfd::MessageDialog::new()
                        .set_title("Xfer64 upload")
                        .set_description(format!("xfer64 upload:\n{e}"))
                        .set_level(rfd::MessageLevel::Error)
                        .show();
                }
                std::process::exit(1);
            }
        }
    }
    xfer64_lib::run();
}
