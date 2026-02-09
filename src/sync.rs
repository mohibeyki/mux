use log::{info, warn};

use crate::history::Shell;
use crate::searcher::HistorySearcher;

/// Result of syncing shell history into the searcher
pub struct SyncResult {
    /// Total number of new commands indexed
    pub total_synced: usize,
    /// Warnings for shells that failed to sync
    pub warnings: Vec<String>,
}

/// Sync history from all supported shells (Zsh, Bash, Fish) into the searcher.
/// Returns the number of new commands indexed and any warnings.
pub fn sync_shell_history(searcher: &mut HistorySearcher) -> SyncResult {
    let sync_start = std::time::Instant::now();
    let shells = [Shell::Zsh, Shell::Bash, Shell::Fish];
    let mut total_synced = 0;
    let mut warnings = Vec::new();

    for shell in shells {
        let shell_start = std::time::Instant::now();
        match searcher.sync_from_shell_history(shell) {
            Ok(count) if count > 0 => {
                info!(
                    "Synced {} commands from {:?} in {:.2?}",
                    count,
                    shell,
                    shell_start.elapsed()
                );
                total_synced += count;
            }
            Ok(_) => {}
            Err(e) => {
                warn!("Failed to sync from {:?}: {}", shell, e);
                warnings.push(format!("Failed to sync {:?} history: {}", shell, e));
            }
        }
    }

    if total_synced > 0 {
        info!(
            "Indexed {} new commands in {:.2?} ({} total)",
            total_synced,
            sync_start.elapsed(),
            searcher.len()
        );
    }

    SyncResult {
        total_synced,
        warnings,
    }
}
