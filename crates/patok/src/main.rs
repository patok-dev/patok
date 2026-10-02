//! `patok` CLI entry point. The only shipped executable (spec Part X, section 3).

use clap::Parser;

#[derive(Parser)]
#[command(name = "patok", version, about)]
struct Cli {}

fn main() {
    Cli::parse();
}
