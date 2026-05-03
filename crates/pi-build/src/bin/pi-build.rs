//! `pi-build` CLI entry point. Per RFD 0028 §A.7 (the
//! `validate` verb). Codegen verbs (`pi-build <toml>`,
//! `--build`, `--target`) ship in Commit B/C.

use std::path::PathBuf;
use std::process::ExitCode;

const USAGE: &str = "usage: pi-build validate <agent.toml>";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [verb, path] if verb == "validate" => run_validate(PathBuf::from(path)),
        [verb] if verb == "validate" => {
            eprintln!("pi-build validate: missing <agent.toml>\n{USAGE}");
            ExitCode::from(64) // EX_USAGE
        }
        _ => {
            eprintln!("{USAGE}");
            ExitCode::from(64)
        }
    }
}

fn run_validate(path: PathBuf) -> ExitCode {
    let raw = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("pi-build validate: cannot read {}: {e}", path.display());
            return ExitCode::from(66); // EX_NOINPUT
        }
    };
    match pi_build::parse(&raw) {
        Ok(m) => {
            // OK: <name> <version> (<provider>/<model>) — N tools allowlisted
            println!(
                "OK: {} {} ({}/{}) — {} tools allowlisted",
                m.agent.name,
                m.agent.version,
                m.provider.name.as_kebab(),
                m.provider.model,
                m.tools.allowlist.len(),
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("pi-build validate: {e}");
            ExitCode::from(65) // EX_DATAERR
        }
    }
}
