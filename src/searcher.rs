use log::{debug, info};
use nucleo_matcher::{Config, Matcher, Utf32String};
use rusqlite::{params, Connection, Result as SqlResult};
use std::path::PathBuf;

use crate::history::{HistoryEntry, HistoryReader, Shell};

/// In-memory command history searcher with persistent SQLite backing
pub struct HistorySearcher {
    /// All indexed commands (sorted by frequency DESC)
    entries: Vec<IndexedCommand>,

    /// Pre-computed Utf32String representations for fuzzy matching (parallel to entries)
    haystacks: Vec<Utf32String>,

    /// Nucleo fuzzy matcher
    matcher: Matcher,

    /// SQLite database connection
    db: Connection,
}

/// A command entry with metadata
#[derive(Debug, Clone)]
pub struct IndexedCommand {
    pub id: i64,
    pub command: String,
    pub frequency: u32,
    pub last_used: Option<i64>,
}

/// Search result with relevance score
#[derive(Debug, Clone)]
pub struct SearchResult {
    pub command: String,
    pub score: u32,
}

impl HistorySearcher {
    /// Create a new HistorySearcher with the given database path
    pub fn new(db_path: PathBuf) -> Result<Self, Box<dyn std::error::Error>> {
        debug!("Opening database at: {}", db_path.display());
        let db = Connection::open(&db_path)?;

        // Initialize schema
        debug!("Initializing database schema");
        Self::init_schema(&db)?;

        // Load data from database
        debug!("Loading commands from database");
        let entries = Self::load_from_db(&db)?;
        info!("Loaded {} commands from database", entries.len());

        let haystacks = entries
            .iter()
            .map(|e| Utf32String::from(e.command.as_str()))
            .collect();

        Ok(Self {
            entries,
            haystacks,
            matcher: Matcher::new(Config::DEFAULT),
            db,
        })
    }

    /// Initialize SQLite schema
    fn init_schema(db: &Connection) -> SqlResult<()> {
        db.execute(
            "CREATE TABLE IF NOT EXISTS commands (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                command TEXT NOT NULL UNIQUE,
                timestamp INTEGER,
                shell_source TEXT NOT NULL,
                frequency INTEGER NOT NULL DEFAULT 1,
                last_used INTEGER,
                created_at INTEGER NOT NULL DEFAULT (strftime('%s', 'now'))
            )",
            [],
        )?;

        // Index for fast lookups
        db.execute(
            "CREATE INDEX IF NOT EXISTS idx_commands_command ON commands(command)",
            [],
        )?;

        db.execute(
            "CREATE INDEX IF NOT EXISTS idx_commands_frequency ON commands(frequency DESC)",
            [],
        )?;

        // Track last sync state per shell
        db.execute(
            "CREATE TABLE IF NOT EXISTS sync_state (
                shell_source TEXT PRIMARY KEY,
                last_sync_timestamp INTEGER NOT NULL,
                last_line_count INTEGER NOT NULL DEFAULT 0
            )",
            [],
        )?;

        Ok(())
    }

    /// Load all commands from database into memory
    fn load_from_db(db: &Connection) -> Result<Vec<IndexedCommand>, Box<dyn std::error::Error>> {
        let mut stmt = db.prepare(
            "SELECT id, command, frequency, last_used
             FROM commands
             ORDER BY frequency DESC, last_used DESC"
        )?;

        let rows = stmt.query_map([], |row| {
            Ok(IndexedCommand {
                id: row.get(0)?,
                command: row.get(1)?,
                frequency: row.get(2)?,
                last_used: row.get(3)?,
            })
        })?;

        let mut entries = Vec::new();
        for row in rows {
            entries.push(row?);
        }

        Ok(entries)
    }

    /// Sync new commands from shell history to database
    pub fn sync_from_shell_history(&mut self, shell: Shell) -> Result<usize, Box<dyn std::error::Error>> {
        debug!("Starting sync from {:?} shell", shell);
        let reader = HistoryReader::new(shell)?;
        let shell_name = format!("{:?}", shell);

        // Get last sync state
        let (last_sync_ts, last_line_count) = self.get_sync_state(&shell_name)?;
        debug!(
            "Last sync for {:?}: timestamp={}, lines={}",
            shell, last_sync_ts, last_line_count
        );

        // Read shell history
        let history = reader.read_history()?;
        let total_lines = history.len();
        debug!("Read {} total commands from {:?} history", total_lines, shell);

        // Filter for new commands:
        // - Entries with timestamps: use timestamp comparison
        // - Entries without timestamps: only process lines beyond the last synced count
        let new_commands: Vec<_> = history
            .into_iter()
            .enumerate()
            .filter(|(i, entry)| {
                if let Some(ts) = entry.timestamp {
                    ts > last_sync_ts
                } else {
                    // No timestamp: only process entries beyond previously synced line count
                    *i >= last_line_count
                }
            })
            .map(|(_, entry)| entry)
            .collect();

        let count = new_commands.len();
        debug!("Found {} new commands from {:?}", count, shell);

        // Insert new commands in a single transaction for performance
        {
            let tx = self.db.transaction()?;
            for entry in &new_commands {
                Self::insert_or_update_command_on(&tx, entry, &shell_name)?;
            }
            Self::update_sync_state_on(&tx, &shell_name, total_lines)?;
            tx.commit()?;
        }

        // Reload in-memory data
        self.reload_from_db()?;

        info!("Synced {} new commands from {:?}", count, shell);

        Ok(count)
    }

    /// Get last sync state for a shell: (last_timestamp, last_line_count)
    fn get_sync_state(&self, shell_source: &str) -> SqlResult<(i64, usize)> {
        let mut stmt = self.db.prepare(
            "SELECT last_sync_timestamp, last_line_count FROM sync_state WHERE shell_source = ?"
        )?;

        match stmt.query_row([shell_source], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)? as usize))
        }) {
            Ok(state) => Ok(state),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok((0, 0)),
            Err(e) => Err(e),
        }
    }

    /// Update sync state for a shell
    fn update_sync_state_on(conn: &Connection, shell_source: &str, line_count: usize) -> SqlResult<()> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;

        conn.execute(
            "INSERT OR REPLACE INTO sync_state (shell_source, last_sync_timestamp, last_line_count)
             VALUES (?, ?, ?)",
            params![shell_source, now, line_count as i64],
        )?;

        Ok(())
    }

    /// Insert or update a command in the database (convenience wrapper for tests)
    #[cfg(test)]
    pub fn insert_or_update_command(&self, entry: &HistoryEntry, shell_source: &str) -> SqlResult<()> {
        Self::insert_or_update_command_on(&self.db, entry, shell_source)
    }

    /// Insert or update a command using a specific connection (or transaction)
    fn insert_or_update_command_on(conn: &Connection, entry: &HistoryEntry, shell_source: &str) -> SqlResult<()> {
        let mut stmt = conn.prepare("SELECT id, frequency FROM commands WHERE command = ?")?;

        match stmt.query_row([&entry.command], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, u32>(1)?))
        }) {
            Ok((id, freq)) => {
                conn.execute(
                    "UPDATE commands SET frequency = ?, last_used = ? WHERE id = ?",
                    params![freq + 1, entry.timestamp, id],
                )?;
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => {
                conn.execute(
                    "INSERT INTO commands (command, timestamp, shell_source, frequency, last_used)
                     VALUES (?, ?, ?, 1, ?)",
                    params![&entry.command, entry.timestamp, shell_source, entry.timestamp],
                )?;
            }
            Err(e) => return Err(e),
        }

        Ok(())
    }

    /// Fuzzy search for commands
    pub fn search(&mut self, query: &str, limit: usize) -> Vec<SearchResult> {
        if query.is_empty() {
            // Return most frequent commands
            return self.entries
                .iter()
                .take(limit)
                .map(|e| SearchResult {
                    command: e.command.clone(),
                    score: e.frequency,
                })
                .collect();
        }

        // Convert query to Utf32String for nucleo matcher
        let query_utf32 = Utf32String::from(query);

        let mut results: Vec<_> = self.entries
            .iter()
            .zip(self.haystacks.iter())
            .filter_map(|(entry, haystack)| {
                let score = self.matcher.fuzzy_match(haystack.slice(..), query_utf32.slice(..))?;

                // Combine fuzzy score with frequency for ranking
                let combined_score = score as u32 + (entry.frequency * 10);

                Some((combined_score, entry))
            })
            .collect();

        // Sort by combined score (descending)
        results.sort_by_key(|(score, _)| std::cmp::Reverse(*score));

        results
            .into_iter()
            .take(limit)
            .map(|(score, entry)| SearchResult {
                command: entry.command.clone(),
                score,
            })
            .collect()
    }

    /// Record command usage (increment frequency, insert if new)
    pub fn record_usage(&mut self, command: &str) -> Result<(), Box<dyn std::error::Error>> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;

        // Try to update existing row
        let rows_updated = self.db.execute(
            "UPDATE commands SET frequency = frequency + 1, last_used = ? WHERE command = ?",
            params![now, command],
        )?;

        if rows_updated == 0 {
            // Command is new -- insert it
            self.db.execute(
                "INSERT INTO commands (command, timestamp, shell_source, frequency, last_used)
                 VALUES (?, ?, 'mux', 1, ?)",
                params![command, now, now],
            )?;

            // Add to in-memory entries
            let id = self.db.last_insert_rowid();
            let entry = IndexedCommand {
                id,
                command: command.to_string(),
                frequency: 1,
                last_used: Some(now),
            };
            self.haystacks.push(Utf32String::from(command));
            self.entries.push(entry);
        } else {
            // Update in-memory entry and bubble up to maintain sort order
            if let Some(mut idx) = self.entries.iter().position(|e| e.command == command) {
                self.entries[idx].frequency += 1;
                self.entries[idx].last_used = Some(now);

                while idx > 0 && self.entries[idx].frequency > self.entries[idx - 1].frequency {
                    self.entries.swap(idx, idx - 1);
                    self.haystacks.swap(idx, idx - 1);
                    idx -= 1;
                }
            }
        }

        Ok(())
    }

    /// Persist all pending changes to database (called on shutdown)
    pub fn flush(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        // Transaction to ensure atomicity
        let tx = self.db.transaction()?;

        for entry in &self.entries {
            tx.execute(
                "UPDATE commands SET frequency = ?, last_used = ? WHERE id = ?",
                params![entry.frequency, entry.last_used, entry.id],
            )?;
        }

        tx.commit()?;
        Ok(())
    }

    /// Reload all in-memory data from the database
    pub fn reload_from_db(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let entries = Self::load_from_db(&self.db)?;
        self.haystacks = entries
            .iter()
            .map(|e| Utf32String::from(e.command.as_str()))
            .collect();
        self.entries = entries;
        Ok(())
    }

    /// Get the most recently used command (by last_used timestamp)
    pub fn most_recent_command(&self) -> Option<&IndexedCommand> {
        self.entries
            .iter()
            .filter(|e| e.last_used.is_some())
            .max_by_key(|e| e.last_used)
    }

    /// Get all commands (for displaying in TUI)
    pub fn get_all_commands(&self) -> &[IndexedCommand] {
        &self.entries
    }

    /// Get command count
    pub fn len(&self) -> usize {
        self.entries.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    #[test]
    fn test_create_searcher() {
        let temp_db = NamedTempFile::new().unwrap();
        let searcher = HistorySearcher::new(temp_db.path().to_path_buf()).unwrap();
        assert_eq!(searcher.len(), 0);
    }

    #[test]
    fn test_insert_and_search() {
        let temp_db = NamedTempFile::new().unwrap();
        let mut searcher = HistorySearcher::new(temp_db.path().to_path_buf()).unwrap();

        // Insert test commands
        let entry = HistoryEntry {
            command: "cargo build".to_string(),
            timestamp: Some(1234567890),
        };

        searcher.insert_or_update_command(&entry, "Bash").unwrap();
        searcher.reload_from_db().unwrap();

        // Search
        let results = searcher.search("build", 10);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].command, "cargo build");
    }

    #[test]
    fn test_record_usage() {
        let temp_db = NamedTempFile::new().unwrap();
        let mut searcher = HistorySearcher::new(temp_db.path().to_path_buf()).unwrap();

        // Insert and record usage
        let entry = HistoryEntry {
            command: "cargo test".to_string(),
            timestamp: Some(1234567890),
        };

        searcher.insert_or_update_command(&entry, "Zsh").unwrap();
        searcher.reload_from_db().unwrap();

        // Record usage twice
        searcher.record_usage("cargo test").unwrap();
        searcher.record_usage("cargo test").unwrap();

        let freq = searcher.get_all_commands().iter()
            .find(|e| e.command == "cargo test")
            .map(|e| e.frequency)
            .unwrap();
        assert_eq!(freq, 3); // 1 initial + 2 uses
    }
}
