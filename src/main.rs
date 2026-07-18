//! columbo CLI entry point.
//!
//! Usage: columbo < infile.gz > outfile.gz
//!
//! Reads from stdin, auto-detects format (gzip or raw deflate),
//! optimises, writes to stdout only if strictly smaller.

use std::io::{self, Read, Write};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut input = Vec::new();
    io::stdin().read_to_end(&mut input)?;

    match columbo::container::detect_and_optimise(&input) {
        Some(optimised) => {
            if optimised.len() < input.len() {
                io::stdout().write_all(&optimised)?;
            } else {
                // No saving — output original unchanged
                io::stdout().write_all(&input)?;
            }
        }
        None => {
            // Unrecognised format — pass through
            io::stdout().write_all(&input)?;
        }
    }

    Ok(())
}
