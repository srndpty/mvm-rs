//! 開発用の検査。`cargo xtask <command>` で実行する。
//!
//! 終了コード: 0 = 成功、1 = 検査違反または実行時エラー、2 = 使い方の誤り。

mod arch;

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("arch") => match arch::run() {
            Ok(()) => {
                println!("arch: PASS");
                ExitCode::SUCCESS
            }
            Err(errors) => {
                for error in &errors {
                    eprintln!("arch: {error}");
                }
                eprintln!("arch: FAIL ({} 件)", errors.len());
                ExitCode::from(1)
            }
        },
        _ => {
            eprintln!("使い方: cargo xtask arch");
            ExitCode::from(2)
        }
    }
}
