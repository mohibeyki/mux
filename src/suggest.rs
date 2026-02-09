use log::{debug, info};
use std::collections::{HashMap, HashSet};

use crate::searcher::{HistorySearcher, IndexedCommand};

// --- Argument parsing types ---

/// A parsed command broken into its command prefix and structured arguments
#[derive(Debug, Clone)]
struct ParsedCommand {
    /// Multi-level prefixes: e.g., ["cargo", "cargo build"]
    prefixes: Vec<String>,
    /// Parsed argument entries
    args: Vec<ParsedArg>,
}

/// A single parsed argument (flag or key-value option)
#[derive(Debug, Clone)]
struct ParsedArg {
    /// The flag itself: "--release", "-j", "--target"
    name: String,
    /// Optional value: None for flags, Some("x86_64") for --target x86_64
    value: Option<String>,
}

/// What kind of token the engine should suggest next
#[derive(Debug, PartialEq)]
enum NextExpected {
    /// Typing the first word (no completed tokens)
    Command,
    /// Completed some prefix words, expecting subcommand or first arg
    Subcommand,
    /// In argument territory, expecting a flag/option
    Argument,
    /// Last completed token was a value-taking arg, expecting its value
    Value(String),
}

/// Context derived from analyzing completed tokens
#[derive(Debug)]
struct InputContext {
    /// Multi-level command prefixes: ["cargo", "cargo build"]
    prefixes: Vec<String>,
    /// What the engine should suggest next
    next_expected: NextExpected,
    /// Arguments already present in the completed tokens (for dedup)
    existing_args: HashSet<String>,
}

/// Parse a complete command string into structured parts (shell-aware tokenization)
fn parse_command(command: &str) -> ParsedCommand {
    let tokens = match shell_words::split(command) {
        Ok(t) => t,
        Err(_) => command.split_whitespace().map(String::from).collect(),
    };
    if tokens.is_empty() {
        return ParsedCommand {
            prefixes: Vec::new(),
            args: Vec::new(),
        };
    }

    // Find where the command prefix ends (first token starting with '-')
    let prefix_end = tokens
        .iter()
        .position(|t| t.starts_with('-'))
        .unwrap_or(tokens.len());

    // Build multi-level prefixes
    let mut prefixes = Vec::new();
    let mut running = String::new();
    for (i, tok) in tokens[..prefix_end].iter().enumerate() {
        if i > 0 {
            running.push(' ');
        }
        running.push_str(tok);
        prefixes.push(running.clone());
    }

    // Parse arguments from the remaining tokens
    let mut args = Vec::new();
    let arg_tokens = &tokens[prefix_end..];
    let mut i = 0;
    while i < arg_tokens.len() {
        let tok = &arg_tokens[i];

        if tok == "--" {
            break;
        }

        if tok.starts_with('-') {
            if let Some(eq_pos) = tok.find('=') {
                args.push(ParsedArg {
                    name: tok[..eq_pos].to_string(),
                    value: Some(tok[eq_pos + 1..].to_string()),
                });
            } else {
                let value = if i + 1 < arg_tokens.len() && !arg_tokens[i + 1].starts_with('-') {
                    i += 1;
                    Some(arg_tokens[i].clone())
                } else {
                    None
                };
                args.push(ParsedArg {
                    name: tok.clone(),
                    value,
                });
            }
        }
        i += 1;
    }

    ParsedCommand { prefixes, args }
}

/// Split input into completed tokens and partial (the token being typed).
/// If input has a trailing space, partial is empty (user finished the last token).
/// Uses shell-aware tokenization for completed tokens, but keeps the raw last
/// token as partial since the user may be mid-quote.
fn split_input(input: &str) -> (Vec<String>, String) {
    let raw_tokens: Vec<&str> = input.split_whitespace().collect();
    if raw_tokens.is_empty() {
        return (Vec::new(), String::new());
    }
    if input.ends_with(' ') {
        // All tokens are complete; use shell-aware parsing
        let tokens = match shell_words::split(input) {
            Ok(t) => t,
            Err(_) => raw_tokens.iter().map(|s| s.to_string()).collect(),
        };
        (tokens, String::new())
    } else {
        // The last raw token is the partial being typed (may be mid-quote)
        let Some(last_token) = raw_tokens.last() else {
            return (Vec::new(), String::new());
        };
        let partial = last_token.to_string();
        // Parse everything before the last whitespace-delimited token
        let prefix = input.trim_end().rsplit_once(char::is_whitespace)
            .map(|(before, _)| before)
            .unwrap_or("");
        let completed = if prefix.is_empty() {
            Vec::new()
        } else {
            match shell_words::split(prefix) {
                Ok(t) => t,
                Err(_) => prefix.split_whitespace().map(String::from).collect(),
            }
        };
        (completed, partial)
    }
}

/// Argument-aware suggestion engine for command input
pub struct SuggestionEngine {
    /// command_prefix -> { arg_name -> frequency }
    /// e.g., "cargo build" -> {"--release": 15, "--target": 5}
    arg_index: HashMap<String, HashMap<String, u32>>,

    /// command_prefix -> { arg_name -> { value -> frequency } }
    /// e.g., "cargo build" -> {"--target" -> {"x86_64": 3, "wasm32": 2}}
    arg_value_index: HashMap<String, HashMap<String, HashMap<String, u32>>>,

    /// arg_name -> { value -> frequency } (global fallback)
    /// e.g., "--target" -> {"x86_64": 5, "wasm32": 3}
    global_arg_values: HashMap<String, HashMap<String, u32>>,

    /// Pre-computed set of args that have been seen with values (O(1) lookup)
    value_taking_args: HashSet<String>,
}

/// A suggestion result
#[derive(Debug, Clone)]
pub struct Suggestion {
    pub text: String,
    pub score: f32,
    pub suggestion_type: SuggestionType,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SuggestionType {
    /// Complete command from history (via fuzzy search)
    FullCommand,
    /// A flag/option for the current command (e.g., --release, --target)
    Argument,
    /// A value for the current argument (e.g., x86_64 for --target)
    ArgumentValue,
}

impl SuggestionEngine {
    /// Create a new suggestion engine from indexed commands
    pub fn new(commands: &[IndexedCommand]) -> Self {
        debug!("Building suggestion engine from {} commands", commands.len());

        let mut arg_index: HashMap<String, HashMap<String, u32>> = HashMap::new();
        let mut arg_value_index: HashMap<String, HashMap<String, HashMap<String, u32>>> =
            HashMap::new();
        let mut global_arg_values: HashMap<String, HashMap<String, u32>> = HashMap::new();

        for cmd in commands {
            let freq_weight = cmd.frequency.max(1);
            let parsed = parse_command(&cmd.command);

            for prefix in &parsed.prefixes {
                for arg in &parsed.args {
                    *arg_index
                        .entry(prefix.clone())
                        .or_default()
                        .entry(arg.name.clone())
                        .or_insert(0) += freq_weight;

                    if let Some(ref value) = arg.value {
                        *arg_value_index
                            .entry(prefix.clone())
                            .or_default()
                            .entry(arg.name.clone())
                            .or_default()
                            .entry(value.clone())
                            .or_insert(0) += freq_weight;

                        *global_arg_values
                            .entry(arg.name.clone())
                            .or_default()
                            .entry(value.clone())
                            .or_insert(0) += freq_weight;
                    }
                }
            }
        }

        // Pre-compute which args take values for O(1) lookups
        let value_taking_args: HashSet<String> = global_arg_values.keys().cloned().collect();

        info!(
            "Suggestion engine built: {} command prefixes indexed",
            arg_index.len()
        );

        Self {
            arg_index,
            arg_value_index,
            global_arg_values,
            value_taking_args,
        }
    }

    /// Incrementally index a single command (called when a new command is submitted)
    pub fn index_command(&mut self, command: &str) {
        let parsed = parse_command(command);

        for prefix in &parsed.prefixes {
            for arg in &parsed.args {
                *self
                    .arg_index
                    .entry(prefix.clone())
                    .or_default()
                    .entry(arg.name.clone())
                    .or_insert(0) += 1;

                if let Some(ref value) = arg.value {
                    *self
                        .arg_value_index
                        .entry(prefix.clone())
                        .or_default()
                        .entry(arg.name.clone())
                        .or_default()
                        .entry(value.clone())
                        .or_insert(0) += 1;

                    *self
                        .global_arg_values
                        .entry(arg.name.clone())
                        .or_default()
                        .entry(value.clone())
                        .or_insert(0) += 1;

                    // Update value_taking_args set
                    self.value_taking_args.insert(arg.name.clone());
                }
            }
        }
    }

    /// Check if an argument has ever been seen with a value in the index (O(1))
    fn arg_takes_value(&self, arg_name: &str) -> bool {
        self.value_taking_args.contains(arg_name)
    }

    /// Analyze completed tokens to determine context and what to suggest next
    fn analyze_completed(&self, completed: &[String]) -> InputContext {
        if completed.is_empty() {
            return InputContext {
                prefixes: Vec::new(),
                next_expected: NextExpected::Command,
                existing_args: HashSet::new(),
            };
        }

        // Find where the command prefix ends (first token starting with '-')
        let prefix_end = completed
            .iter()
            .position(|t| t.starts_with('-'))
            .unwrap_or(completed.len());

        // Build multi-level prefixes
        let mut prefixes = Vec::new();
        let mut running = String::new();
        for (i, tok) in completed[..prefix_end].iter().enumerate() {
            if i > 0 {
                running.push(' ');
            }
            running.push_str(tok);
            prefixes.push(running.clone());
        }

        // If all completed tokens are prefix words, we're still in subcommand territory
        if prefix_end == completed.len() {
            return InputContext {
                prefixes,
                next_expected: NextExpected::Subcommand,
                existing_args: HashSet::new(),
            };
        }

        // Walk the argument tokens to collect existing args and determine what comes next
        let mut existing_args = HashSet::new();
        let mut i = prefix_end;
        while i < completed.len() {
            let tok = &completed[i];

            if tok == "--" {
                // End of options; everything after is positional
                break;
            }

            if tok.starts_with('-') {
                if tok.contains('=') {
                    // --key=value: arg is fully consumed
                    if let Some(eq_pos) = tok.find('=') {
                        existing_args.insert(tok.get(..eq_pos).unwrap_or(tok).to_string());
                    }
                } else {
                    existing_args.insert(tok.to_string());
                    // If this arg takes values and the next token is a non-dash value, consume it
                    if self.arg_takes_value(&tok)
                        && i + 1 < completed.len()
                        && !completed[i + 1].starts_with('-')
                    {
                        i += 1; // skip the value token
                    }
                }
            }
            i += 1;
        }

        // Determine what comes next by looking at the last completed token
        // Safety: completed is guaranteed non-empty (early return at top of function)
        let Some(last) = completed.last() else {
            return InputContext {
                prefixes,
                next_expected: NextExpected::Command,
                existing_args,
            };
        };
        let next_expected = if last.starts_with('-')
            && !last.contains('=')
            && *last != "--"
            && self.arg_takes_value(last)
        {
            // Last completed token is a value-taking arg that hasn't received its value yet
            NextExpected::Value(last.to_string())
        } else {
            NextExpected::Argument
        };

        InputContext {
            prefixes,
            next_expected,
            existing_args,
        }
    }

    /// Get suggestions for the current input
    pub fn suggest(&self, input: &str, searcher: &mut HistorySearcher, limit: usize) -> Vec<Suggestion> {
        let trimmed = input.trim_start();

        if trimmed.is_empty() {
            return Self::commands_from_searcher(searcher, "", limit);
        }

        let (completed, partial) = split_input(trimmed);
        let ctx = self.analyze_completed(&completed);

        match ctx.next_expected {
            NextExpected::Command => {
                Self::commands_from_searcher(searcher, &partial, limit)
            }
            NextExpected::Subcommand => {
                let cmd_results = Self::commands_from_searcher(searcher, trimmed, limit);
                if !cmd_results.is_empty() {
                    return cmd_results;
                }
                if partial.starts_with('-') {
                    self.suggest_args(&ctx.prefixes, &partial, &ctx.existing_args, limit)
                } else {
                    Vec::new()
                }
            }
            NextExpected::Argument => {
                let cmd_results = Self::commands_from_searcher(searcher, trimmed, limit);
                if !cmd_results.is_empty() {
                    return cmd_results;
                }
                self.suggest_args(&ctx.prefixes, &partial, &ctx.existing_args, limit)
            }
            NextExpected::Value(ref arg_name) => {
                let cmd_results = Self::commands_from_searcher(searcher, trimmed, limit);
                if !cmd_results.is_empty() {
                    return cmd_results;
                }
                let val_results =
                    self.suggest_arg_values(&ctx.prefixes, arg_name, &partial, limit);
                if !val_results.is_empty() {
                    return val_results;
                }
                self.suggest_args(&ctx.prefixes, &partial, &ctx.existing_args, limit)
            }
        }
    }

    /// Suggest arguments for the current command prefix
    fn suggest_args(
        &self,
        prefixes: &[String],
        partial: &str,
        exclude: &HashSet<String>,
        limit: usize,
    ) -> Vec<Suggestion> {
        let mut scored: HashMap<String, f32> = HashMap::new();

        for (i, prefix) in prefixes.iter().enumerate() {
            let boost = if i == prefixes.len() - 1 { 2.0 } else { 1.0 };
            if let Some(args) = self.arg_index.get(prefix) {
                for (arg_name, freq) in args {
                    if arg_name.starts_with(partial) && !exclude.contains(arg_name) {
                        let score = *freq as f32 * boost;
                        let entry = scored.entry(arg_name.clone()).or_insert(0.0);
                        *entry = entry.max(score);
                    }
                }
            }
        }

        let mut suggestions: Vec<_> = scored
            .into_iter()
            .map(|(name, score)| Suggestion {
                text: name,
                score,
                suggestion_type: SuggestionType::Argument,
            })
            .collect();

        suggestions.sort_by(|a, b| b.score.total_cmp(&a.score));
        suggestions.truncate(limit);
        suggestions
    }

    /// Suggest values for a specific argument in the context of the current command
    fn suggest_arg_values(
        &self,
        prefixes: &[String],
        arg_name: &str,
        partial: &str,
        limit: usize,
    ) -> Vec<Suggestion> {
        let mut scored: HashMap<String, f32> = HashMap::new();

        // Try command-specific values first
        for (i, prefix) in prefixes.iter().enumerate() {
            let boost = if i == prefixes.len() - 1 { 2.0 } else { 1.5 };
            if let Some(arg_map) = self.arg_value_index.get(prefix) {
                if let Some(values) = arg_map.get(arg_name) {
                    for (value, freq) in values {
                        if value.starts_with(partial) {
                            let score = *freq as f32 * boost;
                            let entry = scored.entry(value.clone()).or_insert(0.0);
                            *entry = entry.max(score);
                        }
                    }
                }
            }
        }

        // Fall back to global values if no command-specific results
        if scored.is_empty() {
            if let Some(values) = self.global_arg_values.get(arg_name) {
                for (value, freq) in values {
                    if value.starts_with(partial) {
                        scored.insert(value.clone(), *freq as f32);
                    }
                }
            }
        }

        let mut suggestions: Vec<_> = scored
            .into_iter()
            .map(|(value, score)| Suggestion {
                text: value,
                score,
                suggestion_type: SuggestionType::ArgumentValue,
            })
            .collect();

        suggestions.sort_by(|a, b| b.score.total_cmp(&a.score));
        suggestions.truncate(limit);
        suggestions
    }

    /// Get command suggestions from the history searcher (fuzzy search)
    fn commands_from_searcher(
        searcher: &mut HistorySearcher,
        query: &str,
        limit: usize,
    ) -> Vec<Suggestion> {
        searcher
            .search(query, limit)
            .into_iter()
            .map(|result| Suggestion {
                text: result.command.clone(),
                score: result.score as f32,
                suggestion_type: SuggestionType::FullCommand,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::searcher::HistorySearcher;
    use tempfile::NamedTempFile;

    // --- Argument parsing tests ---

    #[test]
    fn test_parse_command_simple_flag() {
        let parsed = parse_command("cargo build --release");
        assert_eq!(parsed.prefixes, vec!["cargo", "cargo build"]);
        assert_eq!(parsed.args.len(), 1);
        assert_eq!(parsed.args[0].name, "--release");
        assert_eq!(parsed.args[0].value, None);
    }

    #[test]
    fn test_parse_command_key_value_space() {
        let parsed = parse_command("cargo build --target x86_64");
        assert_eq!(parsed.prefixes, vec!["cargo", "cargo build"]);
        assert_eq!(parsed.args.len(), 1);
        assert_eq!(parsed.args[0].name, "--target");
        assert_eq!(parsed.args[0].value, Some("x86_64".to_string()));
    }

    #[test]
    fn test_parse_command_key_value_equals() {
        let parsed = parse_command("cargo build --target=wasm32");
        assert_eq!(parsed.args.len(), 1);
        assert_eq!(parsed.args[0].name, "--target");
        assert_eq!(parsed.args[0].value, Some("wasm32".to_string()));
    }

    #[test]
    fn test_parse_command_mixed_args() {
        let parsed = parse_command("cargo test --release -j 4 --run sample_run");
        assert_eq!(parsed.prefixes, vec!["cargo", "cargo test"]);
        assert_eq!(parsed.args.len(), 3);

        assert_eq!(parsed.args[0].name, "--release");
        assert_eq!(parsed.args[0].value, None);

        assert_eq!(parsed.args[1].name, "-j");
        assert_eq!(parsed.args[1].value, Some("4".to_string()));

        assert_eq!(parsed.args[2].name, "--run");
        assert_eq!(parsed.args[2].value, Some("sample_run".to_string()));
    }

    #[test]
    fn test_parse_command_bare_double_dash() {
        let parsed = parse_command("cargo test -- --ignored-flag");
        assert_eq!(parsed.prefixes, vec!["cargo", "cargo test"]);
        assert!(parsed.args.is_empty());
    }

    #[test]
    fn test_parse_command_no_args() {
        let parsed = parse_command("ls");
        assert_eq!(parsed.prefixes, vec!["ls"]);
        assert!(parsed.args.is_empty());
    }

    // --- split_input tests ---

    #[test]
    fn test_split_input_mid_word() {
        let (completed, partial) = split_input("cargo build --ta");
        assert_eq!(completed, vec!["cargo", "build"]);
        assert_eq!(partial, "--ta");
    }

    #[test]
    fn test_split_input_trailing_space() {
        let (completed, partial) = split_input("cargo build ");
        assert_eq!(completed, vec!["cargo", "build"]);
        assert_eq!(partial, "");
    }

    #[test]
    fn test_split_input_single_word() {
        let (completed, partial) = split_input("car");
        assert!(completed.is_empty());
        assert_eq!(partial, "car");
    }

    #[test]
    fn test_split_input_empty() {
        let (completed, partial) = split_input("");
        assert!(completed.is_empty());
        assert_eq!(partial, "");
    }

    // --- analyze_completed tests ---

    fn create_arg_test_commands() -> Vec<IndexedCommand> {
        vec![
            IndexedCommand {
                id: 1,
                command: "cargo build --release".to_string(),
                frequency: 10,
                last_used: Some(1000),
            },
            IndexedCommand {
                id: 2,
                command: "cargo build --target x86_64".to_string(),
                frequency: 5,
                last_used: Some(2000),
            },
            IndexedCommand {
                id: 3,
                command: "cargo build --target wasm32".to_string(),
                frequency: 3,
                last_used: Some(3000),
            },
            IndexedCommand {
                id: 4,
                command: "cargo test --run sample_run".to_string(),
                frequency: 7,
                last_used: Some(4000),
            },
            IndexedCommand {
                id: 5,
                command: "cargo test --run integration_test".to_string(),
                frequency: 4,
                last_used: Some(5000),
            },
        ]
    }

    #[test]
    fn test_arg_index_built() {
        let commands = create_arg_test_commands();
        let engine = SuggestionEngine::new(&commands);

        let cargo_build_args = engine.arg_index.get("cargo build").unwrap();
        assert!(cargo_build_args.contains_key("--release"));
        assert!(cargo_build_args.contains_key("--target"));

        let cargo_test_args = engine.arg_index.get("cargo test").unwrap();
        assert!(cargo_test_args.contains_key("--run"));
    }

    #[test]
    fn test_arg_value_index_built() {
        let commands = create_arg_test_commands();
        let engine = SuggestionEngine::new(&commands);

        let target_values = engine
            .arg_value_index
            .get("cargo build")
            .unwrap()
            .get("--target")
            .unwrap();
        assert!(target_values.contains_key("x86_64"));
        assert!(target_values.contains_key("wasm32"));

        let run_values = engine.global_arg_values.get("--run").unwrap();
        assert!(run_values.contains_key("sample_run"));
        assert!(run_values.contains_key("integration_test"));
    }

    #[test]
    fn test_suggest_args_for_command() {
        let commands = create_arg_test_commands();
        let engine = SuggestionEngine::new(&commands);

        let prefixes = vec!["cargo".to_string(), "cargo build".to_string()];
        let exclude = HashSet::new();
        let suggestions = engine.suggest_args(&prefixes, "--", &exclude, 10);

        assert!(!suggestions.is_empty());
        assert!(suggestions
            .iter()
            .any(|s| s.text == "--release" && s.suggestion_type == SuggestionType::Argument));
        assert!(suggestions
            .iter()
            .any(|s| s.text == "--target" && s.suggestion_type == SuggestionType::Argument));
    }

    #[test]
    fn test_suggest_args_with_partial() {
        let commands = create_arg_test_commands();
        let engine = SuggestionEngine::new(&commands);

        let prefixes = vec!["cargo".to_string(), "cargo build".to_string()];
        let exclude = HashSet::new();
        let suggestions = engine.suggest_args(&prefixes, "--ta", &exclude, 10);

        assert_eq!(suggestions.len(), 1);
        assert_eq!(suggestions[0].text, "--target");
    }

    #[test]
    fn test_suggest_args_excludes_existing() {
        let commands = create_arg_test_commands();
        let engine = SuggestionEngine::new(&commands);

        let prefixes = vec!["cargo".to_string(), "cargo build".to_string()];
        let mut exclude = HashSet::new();
        exclude.insert("--release".to_string());

        let suggestions = engine.suggest_args(&prefixes, "--", &exclude, 10);
        assert!(!suggestions.iter().any(|s| s.text == "--release"));
        assert!(suggestions.iter().any(|s| s.text == "--target"));
    }

    #[test]
    fn test_suggest_arg_values() {
        let commands = create_arg_test_commands();
        let engine = SuggestionEngine::new(&commands);

        let prefixes = vec!["cargo".to_string(), "cargo build".to_string()];
        let suggestions = engine.suggest_arg_values(&prefixes, "--target", "", 10);

        assert!(!suggestions.is_empty());
        assert!(suggestions.iter().any(|s| s.text == "x86_64"
            && s.suggestion_type == SuggestionType::ArgumentValue));
        assert!(suggestions.iter().any(|s| s.text == "wasm32"));
    }

    #[test]
    fn test_suggest_arg_values_with_partial() {
        let commands = create_arg_test_commands();
        let engine = SuggestionEngine::new(&commands);

        let prefixes = vec!["cargo".to_string(), "cargo build".to_string()];
        let suggestions = engine.suggest_arg_values(&prefixes, "--target", "x", 10);

        assert_eq!(suggestions.len(), 1);
        assert_eq!(suggestions[0].text, "x86_64");
    }

    // --- analyze_completed tests ---

    /// Helper to convert &str slices to Vec<String> for analyze_completed
    fn strs(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn test_analyze_empty() {
        let engine = SuggestionEngine::new(&create_arg_test_commands());
        let ctx = engine.analyze_completed(&strs(&[]));
        assert_eq!(ctx.next_expected, NextExpected::Command);
    }

    #[test]
    fn test_analyze_subcommand() {
        let engine = SuggestionEngine::new(&create_arg_test_commands());
        let ctx = engine.analyze_completed(&strs(&["cargo"]));
        assert_eq!(ctx.next_expected, NextExpected::Subcommand);
        assert_eq!(ctx.prefixes, vec!["cargo"]);
    }

    #[test]
    fn test_analyze_subcommand_two_words() {
        let engine = SuggestionEngine::new(&create_arg_test_commands());
        let ctx = engine.analyze_completed(&strs(&["cargo", "build"]));
        assert_eq!(ctx.next_expected, NextExpected::Subcommand);
        assert_eq!(ctx.prefixes, vec!["cargo", "cargo build"]);
    }

    #[test]
    fn test_analyze_after_flag() {
        let engine = SuggestionEngine::new(&create_arg_test_commands());
        let ctx = engine.analyze_completed(&strs(&["cargo", "build", "--release"]));
        assert_eq!(ctx.next_expected, NextExpected::Argument);
        assert!(ctx.existing_args.contains("--release"));
    }

    #[test]
    fn test_analyze_after_value_taking_arg() {
        let engine = SuggestionEngine::new(&create_arg_test_commands());
        let ctx = engine.analyze_completed(&strs(&["cargo", "build", "--target"]));
        assert_eq!(
            ctx.next_expected,
            NextExpected::Value("--target".to_string())
        );
    }

    #[test]
    fn test_analyze_after_value_consumed() {
        let engine = SuggestionEngine::new(&create_arg_test_commands());
        let ctx = engine.analyze_completed(&strs(&["cargo", "build", "--target", "x86_64"]));
        assert_eq!(ctx.next_expected, NextExpected::Argument);
        assert!(ctx.existing_args.contains("--target"));
    }

    #[test]
    fn test_analyze_existing_args_tracked() {
        let engine = SuggestionEngine::new(&create_arg_test_commands());
        let ctx = engine.analyze_completed(&strs(&["cargo", "build", "--release", "--target", "x86_64"]));
        assert!(ctx.existing_args.contains("--release"));
        assert!(ctx.existing_args.contains("--target"));
        assert_eq!(ctx.next_expected, NextExpected::Argument);
    }

    // --- Integration tests (suggest via full pipeline) ---

    #[test]
    fn test_suggest_value_after_value_taking_arg() {
        let engine = SuggestionEngine::new(&create_arg_test_commands());
        let temp_db = NamedTempFile::new().unwrap();
        let mut searcher = HistorySearcher::new(temp_db.path().to_path_buf()).unwrap();

        // "cargo test --run " → values for --run
        let suggestions = engine.suggest("cargo test --run ", &mut searcher, 10);
        assert!(suggestions
            .iter()
            .any(|s| s.text == "sample_run" && s.suggestion_type == SuggestionType::ArgumentValue));
    }

    #[test]
    fn test_suggest_value_with_partial() {
        let engine = SuggestionEngine::new(&create_arg_test_commands());
        let temp_db = NamedTempFile::new().unwrap();
        let mut searcher = HistorySearcher::new(temp_db.path().to_path_buf()).unwrap();

        // "cargo test --run sam" → filtered values
        let suggestions = engine.suggest("cargo test --run sam", &mut searcher, 10);
        assert!(suggestions.iter().any(|s| s.text == "sample_run"));
        assert!(!suggestions.iter().any(|s| s.text == "integration_test"));
    }

    #[test]
    fn test_suggest_arg_mid_typing() {
        let engine = SuggestionEngine::new(&create_arg_test_commands());
        let temp_db = NamedTempFile::new().unwrap();
        let mut searcher = HistorySearcher::new(temp_db.path().to_path_buf()).unwrap();

        // "cargo build --re" → --release
        let suggestions = engine.suggest("cargo build --re", &mut searcher, 10);
        assert!(suggestions.iter().any(|s| s.text == "--release"));
    }

    #[test]
    fn test_suggest_args_after_flag() {
        let engine = SuggestionEngine::new(&create_arg_test_commands());
        let temp_db = NamedTempFile::new().unwrap();
        let mut searcher = HistorySearcher::new(temp_db.path().to_path_buf()).unwrap();

        // "cargo build --release " → more args (--release is a flag, NOT value-taking)
        let suggestions = engine.suggest("cargo build --release ", &mut searcher, 10);
        // Should suggest --target, NOT try to suggest values for --release
        assert!(suggestions
            .iter()
            .any(|s| s.text == "--target" && s.suggestion_type == SuggestionType::Argument));
        assert!(!suggestions
            .iter()
            .any(|s| s.suggestion_type == SuggestionType::ArgumentValue));
    }

    #[test]
    fn test_suggest_args_after_value_consumed() {
        let engine = SuggestionEngine::new(&create_arg_test_commands());
        let temp_db = NamedTempFile::new().unwrap();
        let mut searcher = HistorySearcher::new(temp_db.path().to_path_buf()).unwrap();

        // "cargo build --target x86_64 " → more args (value consumed)
        let suggestions = engine.suggest("cargo build --target x86_64 ", &mut searcher, 10);
        assert!(suggestions
            .iter()
            .any(|s| s.suggestion_type == SuggestionType::Argument));
    }

    #[test]
    fn test_suggest_subcommand_fallback() {
        let engine = SuggestionEngine::new(&create_arg_test_commands());
        let temp_db = NamedTempFile::new().unwrap();
        let mut searcher = HistorySearcher::new(temp_db.path().to_path_buf()).unwrap();

        // "cargo " → Subcommand, falls back to searcher
        let suggestions = engine.suggest("cargo ", &mut searcher, 10);
        assert!(suggestions
            .iter()
            .all(|s| s.suggestion_type == SuggestionType::FullCommand));
    }

    #[test]
    fn test_suggest_empty_input() {
        let engine = SuggestionEngine::new(&create_arg_test_commands());
        let temp_db = NamedTempFile::new().unwrap();
        let mut searcher = HistorySearcher::new(temp_db.path().to_path_buf()).unwrap();

        let suggestions = engine.suggest("", &mut searcher, 10);
        assert!(suggestions.is_empty()); // empty searcher
    }

    #[test]
    fn test_suggest_first_word() {
        let engine = SuggestionEngine::new(&create_arg_test_commands());
        let temp_db = NamedTempFile::new().unwrap();
        let mut searcher = HistorySearcher::new(temp_db.path().to_path_buf()).unwrap();

        // "car" → Command, falls back to searcher
        let suggestions = engine.suggest("car", &mut searcher, 10);
        assert!(suggestions
            .iter()
            .all(|s| s.suggestion_type == SuggestionType::FullCommand));
    }
}
