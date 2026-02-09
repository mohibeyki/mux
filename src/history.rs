use std::fs;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Shell {
    Bash,
    Zsh,
    Fish,
}

#[derive(Debug, Clone)]
pub struct HistoryEntry {
    pub command: String,
    pub timestamp: Option<i64>,
}

#[derive(Debug)]
pub struct HistoryReader {
    shell: Shell,
    history_path: PathBuf,
}

impl HistoryReader {
    /// Create a new HistoryReader for a specific shell
    pub fn new(shell: Shell) -> Result<Self, Box<dyn std::error::Error>> {
        let history_path = Self::get_default_history_path(&shell)?;
        Ok(Self {
            shell,
            history_path,
        })
    }

    /// Get the default history file path for a shell
    fn get_default_history_path(shell: &Shell) -> Result<PathBuf, Box<dyn std::error::Error>> {
        let home = std::env::var("HOME").map_err(|_| "HOME environment variable not set")?;

        let path = match shell {
            Shell::Bash => PathBuf::from(home).join(".bash_history"),
            Shell::Zsh => PathBuf::from(home).join(".zsh_history"),
            Shell::Fish => PathBuf::from(home).join(".local/share/fish/fish_history"),
        };

        Ok(path)
    }

    /// Read all history entries from the history file.
    /// Returns an empty vec if the history file doesn't exist (the shell may not be in use).
    pub fn read_history(&self) -> Result<Vec<HistoryEntry>, Box<dyn std::error::Error>> {
        if !self.history_path.exists() {
            return Ok(Vec::new());
        }

        match self.shell {
            Shell::Bash => self.read_bash_history(),
            Shell::Zsh => self.read_zsh_history(),
            Shell::Fish => self.read_fish_history(),
        }
    }

    /// Read bash history file
    /// Format: Simple newline-separated commands, optionally with timestamps if HISTTIMEFORMAT is set
    fn read_bash_history(&self) -> Result<Vec<HistoryEntry>, Box<dyn std::error::Error>> {
        let file = fs::File::open(&self.history_path)?;
        let reader = BufReader::new(file);
        let mut entries = Vec::new();
        let mut lines = reader.lines();

        while let Some(Ok(line)) = lines.next() {
            // Check if line starts with # (timestamp marker)
            if line.starts_with('#') {
                // Try to parse timestamp
                if let Ok(timestamp) = line[1..].trim().parse::<i64>() {
                    // Next line should be the command
                    if let Some(Ok(command)) = lines.next() {
                        entries.push(HistoryEntry {
                            command,
                            timestamp: Some(timestamp),
                        });
                    }
                } else {
                    // It's a comment, treat as command
                    entries.push(HistoryEntry {
                        command: line,
                        timestamp: None,
                    });
                }
            } else {
                // Regular command without timestamp
                entries.push(HistoryEntry {
                    command: line,
                    timestamp: None,
                });
            }
        }

        Ok(entries)
    }

    /// Read zsh history file.
    /// Supports both extended and non-extended formats, including multi-line commands.
    ///
    /// Extended format (EXTENDED_HISTORY):  `: timestamp:duration;command`
    /// Non-extended format:                 `command`
    ///
    /// Multi-line commands use backslash continuation: lines ending with `\` are
    /// joined with the next line (the backslash is replaced with a newline).
    ///
    /// Uses lossy UTF-8 conversion since zsh can write metafied (non-UTF-8) bytes.
    fn read_zsh_history(&self) -> Result<Vec<HistoryEntry>, Box<dyn std::error::Error>> {
        let bytes = fs::read(&self.history_path)?;
        let content = String::from_utf8_lossy(&bytes);
        let mut entries = Vec::new();

        // First pass: join continuation lines (lines ending with '\')
        let mut joined_lines: Vec<String> = Vec::new();
        for line in content.lines() {
            if let Some(current) = joined_lines.last_mut() {
                if current.ends_with('\\') {
                    // Previous line had a continuation — append this line
                    current.pop(); // remove trailing '\'
                    current.push('\n');
                    current.push_str(line);
                    continue;
                }
            }
            joined_lines.push(line.to_string());
        }

        // Second pass: parse each (potentially joined) line
        for line in &joined_lines {
            if line.is_empty() {
                continue;
            }

            if let Some(entry) = Self::parse_zsh_extended_line(line) {
                entries.push(entry);
            } else {
                // Non-extended format: plain command
                entries.push(HistoryEntry {
                    command: line.to_string(),
                    timestamp: None,
                });
            }
        }

        Ok(entries)
    }

    /// Try to parse a line as zsh extended history format: `: timestamp:duration;command`
    /// Returns None if the line doesn't match the extended format.
    fn parse_zsh_extended_line(line: &str) -> Option<HistoryEntry> {
        // Must start with ": " and contain a semicolon
        let rest = line.strip_prefix(": ")?;
        let semicolon_pos = rest.find(';')?;

        let metadata = &rest[..semicolon_pos];
        let command = &rest[semicolon_pos + 1..];

        // Validate metadata looks like "timestamp:duration" (both numeric)
        let mut parts = metadata.split(':');
        let timestamp_str = parts.next()?;
        let _duration_str = parts.next()?;

        // If there are extra colons or the timestamp isn't numeric, this isn't extended format
        if parts.next().is_some() {
            return None;
        }
        let timestamp = timestamp_str.parse::<i64>().ok()?;

        Some(HistoryEntry {
            command: command.to_string(),
            timestamp: Some(timestamp),
        })
    }

    /// Read fish history file
    /// Format: YAML-like with `- cmd:` and `  when:` fields
    fn read_fish_history(&self) -> Result<Vec<HistoryEntry>, Box<dyn std::error::Error>> {
        let content = fs::read_to_string(&self.history_path)?;
        let mut entries = Vec::new();
        let mut current_command: Option<String> = None;
        let mut current_timestamp: Option<i64> = None;

        for line in content.lines() {
            let trimmed = line.trim();

            if trimmed.starts_with("- cmd:") {
                // Save previous entry if exists
                if let Some(cmd) = current_command.take() {
                    entries.push(HistoryEntry {
                        command: cmd,
                        timestamp: current_timestamp.take(),
                    });
                }

                // Extract command
                current_command = Some(trimmed[6..].trim().to_string());
            } else if trimmed.starts_with("when:") {
                // Extract timestamp
                current_timestamp = trimmed[5..].trim().parse::<i64>().ok();
            }
        }

        // Don't forget the last entry
        if let Some(cmd) = current_command {
            entries.push(HistoryEntry {
                command: cmd,
                timestamp: current_timestamp,
            });
        }

        Ok(entries)
    }

}

#[cfg(test)]
impl HistoryReader {
    /// Create a HistoryReader with a custom history file path (test only)
    pub fn with_path(shell: Shell, path: PathBuf) -> Self {
        Self {
            shell,
            history_path: path,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn test_bash_history_simple() {
        let mut temp_file = NamedTempFile::new().unwrap();
        writeln!(temp_file, "ls -la").unwrap();
        writeln!(temp_file, "cd /tmp").unwrap();
        writeln!(temp_file, "echo hello").unwrap();

        let reader = HistoryReader::with_path(Shell::Bash, temp_file.path().to_path_buf());
        let entries = reader.read_history().unwrap();

        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].command, "ls -la");
        assert_eq!(entries[1].command, "cd /tmp");
        assert_eq!(entries[2].command, "echo hello");
    }

    #[test]
    fn test_bash_history_with_timestamps() {
        let mut temp_file = NamedTempFile::new().unwrap();
        writeln!(temp_file, "#1234567890").unwrap();
        writeln!(temp_file, "ls -la").unwrap();
        writeln!(temp_file, "#1234567900").unwrap();
        writeln!(temp_file, "cd /tmp").unwrap();

        let reader = HistoryReader::with_path(Shell::Bash, temp_file.path().to_path_buf());
        let entries = reader.read_history().unwrap();

        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].command, "ls -la");
        assert_eq!(entries[0].timestamp, Some(1234567890));
        assert_eq!(entries[1].command, "cd /tmp");
        assert_eq!(entries[1].timestamp, Some(1234567900));
    }

    #[test]
    fn test_zsh_history_extended() {
        let mut temp_file = NamedTempFile::new().unwrap();
        writeln!(temp_file, ": 1234567890:0;ls -la").unwrap();
        writeln!(temp_file, ": 1234567900:5;cd /tmp").unwrap();

        let reader = HistoryReader::with_path(Shell::Zsh, temp_file.path().to_path_buf());
        let entries = reader.read_history().unwrap();

        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].command, "ls -la");
        assert_eq!(entries[0].timestamp, Some(1234567890));
        assert_eq!(entries[1].command, "cd /tmp");
        assert_eq!(entries[1].timestamp, Some(1234567900));
    }

    #[test]
    fn test_zsh_history_non_extended() {
        let mut temp_file = NamedTempFile::new().unwrap();
        writeln!(temp_file, "ls -la").unwrap();
        writeln!(temp_file, "cd /tmp").unwrap();
        writeln!(temp_file, "echo hello world").unwrap();

        let reader = HistoryReader::with_path(Shell::Zsh, temp_file.path().to_path_buf());
        let entries = reader.read_history().unwrap();

        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].command, "ls -la");
        assert!(entries[0].timestamp.is_none());
        assert_eq!(entries[1].command, "cd /tmp");
        assert_eq!(entries[2].command, "echo hello world");
    }

    #[test]
    fn test_zsh_history_multiline_extended() {
        let mut temp_file = NamedTempFile::new().unwrap();
        // Multi-line command: for loop with continuation lines
        writeln!(temp_file, ": 1234567890:0;for f in *.txt; do\\").unwrap();
        writeln!(temp_file, "echo $f\\").unwrap();
        writeln!(temp_file, "done").unwrap();
        // Normal command after
        writeln!(temp_file, ": 1234567900:0;ls -la").unwrap();

        let reader = HistoryReader::with_path(Shell::Zsh, temp_file.path().to_path_buf());
        let entries = reader.read_history().unwrap();

        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].command, "for f in *.txt; do\necho $f\ndone");
        assert_eq!(entries[0].timestamp, Some(1234567890));
        assert_eq!(entries[1].command, "ls -la");
        assert_eq!(entries[1].timestamp, Some(1234567900));
    }

    #[test]
    fn test_zsh_history_multiline_non_extended() {
        let mut temp_file = NamedTempFile::new().unwrap();
        writeln!(temp_file, "for f in *.txt; do\\").unwrap();
        writeln!(temp_file, "echo $f\\").unwrap();
        writeln!(temp_file, "done").unwrap();
        writeln!(temp_file, "ls -la").unwrap();

        let reader = HistoryReader::with_path(Shell::Zsh, temp_file.path().to_path_buf());
        let entries = reader.read_history().unwrap();

        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].command, "for f in *.txt; do\necho $f\ndone");
        assert!(entries[0].timestamp.is_none());
        assert_eq!(entries[1].command, "ls -la");
    }

    #[test]
    fn test_zsh_history_mixed_format() {
        let mut temp_file = NamedTempFile::new().unwrap();
        // Some plain commands (before EXTENDED_HISTORY was enabled)
        writeln!(temp_file, "echo old command").unwrap();
        writeln!(temp_file, "cd /var/log").unwrap();
        // Then extended format entries
        writeln!(temp_file, ": 1234567890:0;ls -la").unwrap();
        writeln!(temp_file, ": 1234567900:0;pwd").unwrap();

        let reader = HistoryReader::with_path(Shell::Zsh, temp_file.path().to_path_buf());
        let entries = reader.read_history().unwrap();

        assert_eq!(entries.len(), 4);
        assert_eq!(entries[0].command, "echo old command");
        assert!(entries[0].timestamp.is_none());
        assert_eq!(entries[1].command, "cd /var/log");
        assert!(entries[1].timestamp.is_none());
        assert_eq!(entries[2].command, "ls -la");
        assert_eq!(entries[2].timestamp, Some(1234567890));
        assert_eq!(entries[3].command, "pwd");
        assert_eq!(entries[3].timestamp, Some(1234567900));
    }

    #[test]
    fn test_zsh_history_command_with_semicolon() {
        let mut temp_file = NamedTempFile::new().unwrap();
        // Command itself contains semicolons
        writeln!(temp_file, ": 1234567890:0;echo a; echo b; echo c").unwrap();

        let reader = HistoryReader::with_path(Shell::Zsh, temp_file.path().to_path_buf());
        let entries = reader.read_history().unwrap();

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].command, "echo a; echo b; echo c");
        assert_eq!(entries[0].timestamp, Some(1234567890));
    }

    #[test]
    fn test_fish_history() {
        let mut temp_file = NamedTempFile::new().unwrap();
        writeln!(temp_file, "- cmd: ls -la").unwrap();
        writeln!(temp_file, "  when: 1234567890").unwrap();
        writeln!(temp_file, "- cmd: cd /tmp").unwrap();
        writeln!(temp_file, "  when: 1234567900").unwrap();

        let reader = HistoryReader::with_path(Shell::Fish, temp_file.path().to_path_buf());
        let entries = reader.read_history().unwrap();

        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].command, "ls -la");
        assert_eq!(entries[0].timestamp, Some(1234567890));
        assert_eq!(entries[1].command, "cd /tmp");
        assert_eq!(entries[1].timestamp, Some(1234567900));
    }
}
