use clap::Parser;

/// Command-line argument parser for mux
#[derive(Parser, Debug)]
#[command(name = "mux")]
#[command(author, version, about, long_about = None)]
pub struct Args {
    /// Rebuild the index by deleting the database and re-syncing from shell history
    #[arg(long)]
    pub rebuild: bool,
}

impl Args {
    /// Parse command-line arguments
    pub fn parse_args() -> Self {
        Self::parse()
    }
}
