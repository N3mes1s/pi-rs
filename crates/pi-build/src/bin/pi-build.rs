//! `pi-build` CLI entry — RFD 0028 §A.7 (validate verb) +
//! §B.1 (codegen verb) + §C.2 (build flags).

use std::path::PathBuf;
use std::process::ExitCode;

const USAGE: &str = "usage:
  pi-build validate <agent.toml>
  pi-build <agent.toml> [--out DIR] [--force] [--build] [--target T] [--release | --debug]

  exit codes (per RFD 0028 §Cross-cutting #5):
    0   success
   64   bad CLI usage (EX_USAGE)
   65   manifest parse / validation failed (EX_DATAERR)
   66   cannot read input file (EX_NOINPUT)
   73   I/O error writing the output dir (EX_CANTCREAT)
   75   cargo build failed (EX_TEMPFAIL)";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        eprintln!("{USAGE}");
        return ExitCode::from(64);
    }

    // `validate <toml>` form (Commit A).
    if args[0] == "validate" {
        if args.len() != 2 {
            eprintln!("pi-build validate: expects exactly one path argument\n{USAGE}");
            return ExitCode::from(64);
        }
        return run_validate(PathBuf::from(&args[1]));
    }

    // Codegen form: `pi-build <agent.toml> [opts]` (Commit B).
    let path = PathBuf::from(&args[0]);
    let opts = match parse_build_opts(&args[1..], &path) {
        Ok(o) => o,
        Err(msg) => {
            eprintln!("pi-build: {msg}\n{USAGE}");
            return ExitCode::from(64);
        }
    };
    run_build(path, opts)
}

fn run_validate(path: PathBuf) -> ExitCode {
    let raw = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("pi-build validate: cannot read {}: {e}", path.display());
            return ExitCode::from(66);
        }
    };
    match pi_build::parse(&raw) {
        Ok(m) => {
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
            ExitCode::from(65)
        }
    }
}

fn parse_build_opts(args: &[String], _path: &std::path::Path) -> Result<BuildArgs, String> {
    let mut out_dir: Option<PathBuf> = None;
    let mut force = false;
    let mut build = false;
    let mut target: Option<String> = None;
    let mut release = true;
    let mut iter = args.iter();
    while let Some(a) = iter.next() {
        match a.as_str() {
            "--out" => {
                let v = iter.next().ok_or("--out requires an argument")?;
                out_dir = Some(PathBuf::from(v));
            }
            "--force" => force = true,
            "--build" => build = true,
            "--release" => {
                release = true;
                build = true;
            }
            "--debug" => {
                release = false;
                build = true;
            }
            "--target" => {
                let v = iter.next().ok_or("--target requires an argument")?;
                target = Some(v.clone());
                build = true;
            }
            other => return Err(format!("unknown argument {other:?}")),
        }
    }
    Ok(BuildArgs {
        out_dir,
        force,
        build,
        target,
        release,
    })
}

struct BuildArgs {
    out_dir: Option<PathBuf>,
    force: bool,
    build: bool,
    target: Option<String>,
    release: bool,
}

fn run_build(path: PathBuf, args: BuildArgs) -> ExitCode {
    let raw = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("pi-build: cannot read {}: {e}", path.display());
            return ExitCode::from(66);
        }
    };
    let manifest = match pi_build::parse(&raw) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("pi-build: {e}");
            return ExitCode::from(65);
        }
    };
    let mut opts = pi_build::BuildOptions::defaults_for(&manifest.agent.name);
    if let Some(o) = args.out_dir {
        opts.out_dir = o;
    }
    opts.force = args.force;
    opts.build = args.build;
    opts.target = args.target;
    opts.release = args.release;

    let tree = pi_build::render(&manifest, &raw, pi_build::PI_BUILD_VERSION);
    if let Err(e) = pi_build::write_tree(&tree, &opts) {
        eprintln!("pi-build: {e}");
        return match e {
            pi_build::BuildError::OutDirNotEmpty(_) => ExitCode::from(73),
            pi_build::BuildError::Io { .. } => ExitCode::from(73),
            // Other variants don't surface during write_tree.
            _ => ExitCode::from(1),
        };
    }
    println!("Wrote {}", opts.out_dir.display());

    if !opts.build {
        return ExitCode::SUCCESS;
    }

    // Run cargo build via a tokio runtime.
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("pi-build: tokio runtime init failed: {e}");
            return ExitCode::from(1);
        }
    };
    match rt.block_on(pi_build::cargo_build(&opts)) {
        Ok(outcome) => {
            println!("Built {}", outcome.binary_path.display());
            ExitCode::SUCCESS
        }
        Err(pi_build::BuildError::CargoNotFound) => {
            eprintln!("pi-build: cargo not found on PATH");
            ExitCode::from(75)
        }
        Err(pi_build::BuildError::CargoFailed(status)) => {
            eprintln!("pi-build: cargo build failed with {status}");
            ExitCode::from(75)
        }
        Err(e) => {
            eprintln!("pi-build: {e}");
            ExitCode::from(75)
        }
    }
}
