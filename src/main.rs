mod args;
mod config;
mod history;
mod keymap;
mod logger;
mod parallel;
mod paths;
mod runner;
mod searcher;
mod suggest;
mod sync;
mod tui;

use args::Args;
use config::Config;
use log::{error, info};
use searcher::HistorySearcher;
use suggest::SuggestionEngine;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = match paths::get_config_path() {
        Ok(path) => Config::load(&path),
        Err(_) => Config::default(),
    };

    if let Err(e) = logger::init_logger(&config.logging) {
        eprintln!("Failed to initialize logger: {}", e);
    }

    info!("Config loaded: {:?}", config);

    let args = Args::parse_args();
    let db_path = paths::get_db_path()?;

    // Handle --rebuild: delete existing database to force a full re-sync
    if args.rebuild {
        if db_path.exists() {
            std::fs::remove_file(&db_path)?;
            info!("Rebuilding index: removed existing database");
        }
    }

    let mut searcher = HistorySearcher::new(db_path)?;
    let sync_result = sync::sync_shell_history(&mut searcher);

    let suggestion_engine = SuggestionEngine::new(searcher.get_all_commands());
    let result = tui::run_tui(searcher, suggestion_engine, sync_result.warnings, config).await;

    match result {
        Ok(mut searcher) => {
            searcher.flush()?;
            Ok(())
        }
        Err(e) => {
            error!("TUI error: {}", e);
            Err(e)
        }
    }
}
