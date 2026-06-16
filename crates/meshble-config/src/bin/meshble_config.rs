//! `meshble-config <check|print>` — validate the effective configuration, or print it with every
//! secret redacted. Config file path from `$MESHBLE_CONFIG` or `./meshble.toml`.

use std::path::PathBuf;
use std::process::ExitCode;

use meshble_config::Settings;

fn main() -> ExitCode {
    let cmd = std::env::args().nth(1).unwrap_or_default();
    let path = std::env::var("MESHBLE_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("meshble.toml"));

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
            eprintln!("usage: meshble-config <check|print>  (config: {})", path.display());
            ExitCode::FAILURE
        }
    }
}
