//! ap64-patch <seed> [-o <out.z64>] [--check]
//!
//! Detect the game, verify the seed against its profile, and write the seed with the
//! agent spliced in (by default beside it, as `<stem>-agent.z64`). `--check` only verifies.

use std::path::PathBuf;
use std::process::ExitCode;

use ap64_core::{apply, builtin, default_output, detect, ApplyError, Report};

fn print_report(r: &Report) {
    println!("{} ({}) — {}", r.game, r.release, r.randomizer);
    for c in &r.checks {
        println!(
            "  [{}] {}: {}",
            if c.ok { " ok " } else { "FAIL" },
            c.label,
            c.detail
        );
        if !c.hint.is_empty() {
            println!("         {}", c.hint);
        }
    }
}

fn run() -> Result<(), String> {
    let mut args = std::env::args_os().skip(1);
    let (mut input, mut output, mut check_only) = (None, None, false);
    while let Some(a) = args.next() {
        match a.to_str() {
            Some("-o") | Some("--out") => {
                output = Some(PathBuf::from(args.next().ok_or("-o needs a path")?))
            }
            Some("--check") => check_only = true,
            Some("-h") | Some("--help") => {
                println!("usage: ap64-patch <seed> [-o <out.z64>] [--check]");
                return Ok(());
            }
            _ if input.is_none() => input = Some(PathBuf::from(a)),
            _ => return Err(format!("unexpected argument {a:?}")),
        }
    }
    let input = input.ok_or("usage: ap64-patch <seed> [-o <out.z64>] [--check]")?;

    let bundles = builtin()?;
    let mut rom = std::fs::read(&input).map_err(|e| format!("{}: {e}", input.display()))?;
    let d = detect(&bundles, &mut rom).map_err(|e| format!("{}: {e}", input.display()))?;
    let h = d.header.as_ref();
    println!(
        "{}: {} [{} v{}], {}, boot code {}",
        input.display(),
        h.map_or("?", |h| h.name.as_str()),
        h.map_or("?", |h| h.game_code.as_str()),
        h.map_or(0, |h| h.version),
        d.byte_order,
        d.cic.as_deref().unwrap_or("not a known retail one")
    );
    if d.candidates.is_empty() {
        let known: Vec<_> = bundles
            .iter()
            .map(|b| format!("{} {}", b.profile.name, b.profile.release))
            .collect();
        return Err(format!(
            "no profile for this game (supported: {})",
            known.join(", ")
        ));
    }
    for r in &d.candidates {
        print_report(r);
    }
    let chosen = d
        .chosen()
        .ok_or("no profile passes every check; nothing written")?;
    if check_only {
        return Ok(());
    }
    let bundle = bundles
        .iter()
        .find(|b| b.profile.id == chosen.profile_id)
        .ok_or("profile vanished")?;
    let patched = apply(bundle, &rom).map_err(|e: ApplyError| e.to_string())?;
    for line in &patched.summary {
        println!("  {line}");
    }
    let out = output.unwrap_or_else(|| default_output(&input));
    std::fs::write(&out, &patched.rom).map_err(|e| format!("{}: {e}", out.display()))?;
    println!(
        "wrote {} ({} bytes)\nsha1 {}",
        out.display(),
        patched.rom.len(),
        patched.sha1
    );
    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
