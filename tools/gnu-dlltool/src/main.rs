//! Stand-in for GNU dlltool on windows-gnu without MSYS2 (rustup ships no GNU assembler):
//! forwards to llvm-dlltool, which writes the import library itself.
use std::env;
use std::path::PathBuf;
use std::process::{Command, ExitCode};

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let mut forwarded = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            // Options llvm-dlltool understands, each followed by a value.
            "-d" | "-D" | "-l" | "-m" => {
                if let Some(value) = args.get(i + 1) {
                    forwarded.push(args[i].clone());
                    forwarded.push(value.clone());
                }
                i += 2;
            }
            // GNU-only options: assembler flags and its temp-file prefix.
            "-f" | "--temp-prefix" => i += 2,
            _ => i += 1,
        }
    }
    let tool = env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("llvm-dlltool.exe")))
        .unwrap_or_else(|| PathBuf::from("llvm-dlltool.exe"));
    match Command::new(&tool).args(&forwarded).status() {
        Ok(status) if status.success() => ExitCode::SUCCESS,
        Ok(status) => ExitCode::from(status.code().unwrap_or(1).clamp(1, 255) as u8),
        Err(err) => {
            eprintln!("dlltool shim: cannot run {}: {err}", tool.display());
            ExitCode::FAILURE
        }
    }
}
