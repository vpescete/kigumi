//! `kigumi-config <check|print>` — validate the effective configuration, or print it with every
//! secret redacted. Config file path from `$KIGUMI_CONFIG` or `./kigumi.toml`.

use std::path::PathBuf;
use std::process::ExitCode;

use kigumi_config::Settings;

fn main() -> ExitCode {
    let cmd = std::env::args().nth(1).unwrap_or_default();
    let path = std::env::var("KIGUMI_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("kigumi.toml"));

    match cmd.as_str() {
        "check" => match Settings::load(Some(&path)) {
            Ok(_) => {
                println!("ok: configuration is valid");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("error: {e}");
                ExitCode::FAILURE
            }
        },
        "print" => match Settings::load(Some(&path)) {
            Ok(s) => {
                print!("{}", s.redacted());
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("error: {e}");
                ExitCode::FAILURE
            }
        },
        _ => {
            eprintln!("usage: kigumi-config <check|print>  (config: {})", path.display());
            ExitCode::FAILURE
        }
    }
}
