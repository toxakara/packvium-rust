//! Read a packing request as JSON on stdin, write the result as JSON on stdout.
//!
//! This mirrors the Python and PHP CLIs so the cross-language conformance runner can
//! drive all implementations through one interface.
//!
//!     cargo run -q -p packvium-core --example pack-stdin < request.json

use std::io::{Read, Write};

fn main() -> std::process::ExitCode {
    let mut input = String::new();
    if let Err(error) = std::io::stdin().read_to_string(&mut input) {
        eprintln!("pack-stdin: cannot read stdin: {error}");
        return std::process::ExitCode::from(2);
    }
    match packvium_core::pack_json(&input) {
        Ok(output) => {
            let mut stdout = std::io::stdout();
            if stdout.write_all(output.as_bytes()).is_err() {
                return std::process::ExitCode::from(2);
            }
            let _ = stdout.write_all(b"\n");
            std::process::ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("pack-stdin: {error}");
            std::process::ExitCode::from(1)
        }
    }
}
