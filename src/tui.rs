use crossterm::{
    event::{Event, EventStream},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use futures::StreamExt;
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, List, ListItem, Paragraph},
};
use std::collections::{HashMap, VecDeque};
use std::io;
use std::time::Instant;

use crate::config::Config;
use crate::keymap;
use crate::runner::{OutputMessage, TaskRunner};
use crate::searcher::HistorySearcher;
use crate::suggest::{Suggestion, SuggestionEngine};

// --- Byte-aware cursor helpers ---

fn prev_char_pos(s: &str, pos: usize) -> usize {
    let before = s.get(..pos).unwrap_or(s);
    before
        .char_indices()
        .next_back()
        .map(|(i, _)| i)
        .unwrap_or(0)
}

fn next_char_pos(s: &str, pos: usize) -> usize {
    let after = s.get(pos..).unwrap_or("");
    after
        .char_indices()
        .nth(1)
        .map(|(i, _)| pos + i)
        .unwrap_or(s.len())
}

fn find_prev_word_boundary(s: &str, pos: usize) -> usize {
    let before = s.get(..pos).unwrap_or(s);
    let trimmed = before.trim_end();
    if trimmed.is_empty() {
        return 0;
    }
    match trimmed.rfind(char::is_whitespace) {
        Some(ws_pos) => {
            let ws_char_len = trimmed.get(ws_pos..)
                .and_then(|s| s.chars().next())
                .map_or(1, |c| c.len_utf8());
            ws_pos + ws_char_len
        }
        None => 0,
    }
}

fn find_next_word_boundary(s: &str, pos: usize) -> usize {
    let after = s.get(pos..).unwrap_or("");
    let after_word = after.trim_start_matches(|c: char| !c.is_whitespace());
    let after_ws = after_word.trim_start();
    pos + (after.len() - after_ws.len())
}

fn extract_first_word(text: &str) -> &str {
    let after_leading_ws = text.trim_start();
    let word_end = after_leading_ws
        .find(char::is_whitespace)
        .unwrap_or(after_leading_ws.len());
    let total_bytes = (text.len() - after_leading_ws.len()) + word_end;
    &text[..total_bytes]
}

// Output display settings — configured via Config, stored in App.

/// A single line of output from a running task
pub struct OutputLine {
    pub runner_label: String,
    pub stream: crate::runner::StreamType,
    pub content: String,
}

pub struct App {
    input: String,
    output: VecDeque<OutputLine>,
    scroll_offset: usize,
    auto_scroll: bool,
    cursor_position: usize,
    searcher: HistorySearcher,
    suggestion_engine: SuggestionEngine,
    suggestions: Vec<Suggestion>,
    selected_suggestion: usize,
    last_quit_press: Option<Instant>,
    /// Track when each task started for runtime display
    task_start_times: HashMap<crate::runner::TaskId, Instant>,
    /// Buffered output for parallel tasks (flushed on completion)
    pending_output: HashMap<crate::runner::TaskId, Vec<OutputLine>>,
    /// Parallel run progress: (completed, total). Reset on each new parallel submission.
    parallel_progress: Option<(usize, usize)>,
    // --- Config values ---
    max_output_lines: usize,
    box_pad_h: usize,
    box_pad_v: usize,
}

impl App {
    pub fn new(searcher: HistorySearcher, suggestion_engine: SuggestionEngine, config: &Config) -> Self {
        Self {
            input: String::new(),
            output: VecDeque::new(),
            scroll_offset: 0,
            auto_scroll: true,
            cursor_position: 0,
            searcher,
            suggestion_engine,
            suggestions: Vec::new(),
            selected_suggestion: 0,
            last_quit_press: None,
            task_start_times: HashMap::new(),
            pending_output: HashMap::new(),
            parallel_progress: None,
            max_output_lines: config.output.max_lines,
            box_pad_h: config.output.box_padding_horizontal,
            box_pad_v: config.output.box_padding_vertical,
        }
    }

    // --- Read accessors ---

    pub fn input(&self) -> &str {
        &self.input
    }

    pub fn cursor_position(&self) -> usize {
        self.cursor_position
    }

    pub fn output(&self) -> &VecDeque<OutputLine> {
        &self.output
    }

    pub fn scroll_offset(&self) -> usize {
        self.scroll_offset
    }

    pub fn has_suggestions(&self) -> bool {
        !self.suggestions.is_empty()
    }

    pub fn suggestions(&self) -> &[Suggestion] {
        &self.suggestions
    }

    /// Consume the App and return the HistorySearcher for shutdown flush
    pub fn into_searcher(self) -> HistorySearcher {
        self.searcher
    }

    // --- Input editing ---

    pub fn insert_char(&mut self, c: char) {
        self.input.insert(self.cursor_position, c);
        self.cursor_position += c.len_utf8();
        self.update_suggestions();
    }

    pub fn delete_char_backward(&mut self) {
        if self.cursor_position > 0 {
            let prev = prev_char_pos(&self.input, self.cursor_position);
            self.input.remove(prev);
            self.cursor_position = prev;
            self.update_suggestions();
        }
    }

    pub fn delete_char_forward(&mut self) {
        if self.cursor_position < self.input.len() {
            self.input.remove(self.cursor_position);
            self.update_suggestions();
        }
    }

    pub fn delete_word_backward(&mut self) {
        let word_start = find_prev_word_boundary(&self.input, self.cursor_position);
        if word_start < self.cursor_position {
            self.input.drain(word_start..self.cursor_position);
            self.cursor_position = word_start;
            self.update_suggestions();
        }
    }

    pub fn delete_word_forward(&mut self) {
        let word_end = find_next_word_boundary(&self.input, self.cursor_position);
        if word_end > self.cursor_position {
            self.input.drain(self.cursor_position..word_end);
            self.update_suggestions();
        }
    }

    pub fn delete_to_line_start(&mut self) {
        if self.cursor_position > 0 {
            self.input.drain(..self.cursor_position);
            self.cursor_position = 0;
            self.update_suggestions();
        }
    }

    pub fn delete_to_line_end(&mut self) {
        if self.cursor_position < self.input.len() {
            self.input.truncate(self.cursor_position);
            self.update_suggestions();
        }
    }

    // --- Cursor movement ---

    pub fn move_cursor_left(&mut self) {
        if self.cursor_position > 0 {
            self.cursor_position = prev_char_pos(&self.input, self.cursor_position);
        }
    }

    pub fn move_cursor_right(&mut self) {
        if self.cursor_position < self.input.len() {
            self.cursor_position = next_char_pos(&self.input, self.cursor_position);
        }
    }

    pub fn move_cursor_word_left(&mut self) {
        self.cursor_position = find_prev_word_boundary(&self.input, self.cursor_position);
    }

    pub fn move_cursor_word_right(&mut self) {
        self.cursor_position = find_next_word_boundary(&self.input, self.cursor_position);
    }

    pub fn move_cursor_home(&mut self) {
        self.cursor_position = 0;
    }

    pub fn move_cursor_end(&mut self) {
        self.cursor_position = self.input.len();
    }

    /// Accept the next word from the suggestion preview (Right arrow at end of input)
    pub fn accept_next_preview_word(&mut self) {
        if self.cursor_position == self.input.len() {
            if let Some(preview) = self.get_suggestion_preview() {
                let next_word = extract_first_word(&preview);
                if !next_word.is_empty() {
                    self.input.push_str(next_word);
                    self.cursor_position = self.input.len();
                    self.update_suggestions();
                }
            }
        }
    }

    // --- Command submission ---

    /// Submit the current input. Returns true if the app should quit (internal commands).
    pub fn submit_command(&mut self, runner: &mut TaskRunner) -> bool {
        if self.input.is_empty() {
            return false;
        }

        let trimmed = self.input.trim();

        // Internal commands
        if trimmed == "exit" || trimmed == "quit" {
            return true;
        }

        if let Err(e) = self.searcher.record_usage(&self.input) {
            log::warn!("Failed to record command usage: {}", e);
        }
        self.suggestion_engine.index_command(&self.input);

        // Check for parallel expansion syntax: [name=range] command {name}
        if let Some(parsed) = crate::parallel::parse_parallel(trimmed) {
            let expanded = crate::parallel::expand(&parsed);
            let total = expanded.len();
            log::info!("Parallel execution: {} tasks", total);
            self.parallel_progress = Some((0, total));
            for cmd in expanded {
                runner.spawn_labeled(&cmd.command, &cmd.label);
            }
        } else {
            runner.spawn_labeled(&self.input, "");
        }

        self.input.clear();
        self.cursor_position = 0;
        // Reset scroll to bottom so new output is visible
        self.auto_scroll = true;
        self.scroll_to_bottom();
        false
    }

    /// Receive output from a running task and append to the output buffer.
    /// All tasks are buffered per-task and flushed as boxed blocks on completion.
    pub fn push_output(&mut self, msg: OutputMessage) {
        if msg.stream == crate::runner::StreamType::Status {
            if msg.content == "started" {
                self.task_start_times.insert(msg.task_id, Instant::now());
                return;
            }

            // Task completed -- compute runtime
            let runtime = self
                .task_start_times
                .remove(&msg.task_id)
                .map(|start| {
                    let dur = start.elapsed();
                    if dur.as_secs() >= 60 {
                        format!(
                            "{}m{:.1}s",
                            dur.as_secs() / 60,
                            dur.as_secs_f64() % 60.0
                        )
                    } else {
                        format!("{:.2}s", dur.as_secs_f64())
                    }
                })
                .unwrap_or_default();

            // Top border: ┌─ [n=1] ─┐ or ┌──────────┐ (no label for single commands)
            let top_label = if msg.runner_label.is_empty() {
                String::new()
            } else {
                msg.runner_label.clone()
            };
            self.append_output(OutputLine {
                runner_label: format!("\x00top:{}", top_label),
                stream: crate::runner::StreamType::Status,
                content: String::new(),
            });

            // Top padding
            for _ in 0..self.box_pad_v {
                self.append_output(OutputLine {
                    runner_label: "\x00box".to_string(),
                    stream: crate::runner::StreamType::Output,
                    content: String::new(),
                });
            }

            // Flush buffered content lines
            if let Some(buffered) = self.pending_output.remove(&msg.task_id) {
                for mut line in buffered {
                    line.runner_label = "\x00box".to_string();
                    self.append_output(line);
                }
            }

            // Bottom padding
            for _ in 0..self.box_pad_v {
                self.append_output(OutputLine {
                    runner_label: "\x00box".to_string(),
                    stream: crate::runner::StreamType::Output,
                    content: String::new(),
                });
            }

            // Bottom border with runtime
            self.append_output(OutputLine {
                runner_label: "\x00bot".to_string(),
                stream: crate::runner::StreamType::Status,
                content: runtime,
            });

            // Update parallel progress if active
            if let Some((ref mut completed, _)) = self.parallel_progress {
                *completed += 1;
            }
        } else {
            // Buffer output for this task
            self.pending_output
                .entry(msg.task_id)
                .or_default()
                .push(OutputLine {
                    runner_label: msg.runner_label,
                    stream: msg.stream,
                    content: msg.content,
                });
        }
    }

    /// Append a line to the output buffer with cap enforcement and auto-scroll
    fn append_output(&mut self, line: OutputLine) {
        self.output.push_back(line);

        while self.output.len() > self.max_output_lines {
            self.output.pop_front();
            self.scroll_offset = self.scroll_offset.saturating_sub(1);
        }

        if self.auto_scroll {
            self.scroll_to_bottom();
        }
    }

    /// Set scroll_offset so the bottom of the output is visible.
    /// Called with the actual panel height during rendering; defaults to a large value
    /// so the rendering logic clamps it.
    fn scroll_to_bottom(&mut self) {
        // Set to a value that rendering will clamp to show the last page
        self.scroll_offset = usize::MAX;
    }

    pub fn clear_output(&mut self) {
        self.output.clear();
        self.scroll_offset = 0;
        self.auto_scroll = true;
    }

    /// Add a warning message to the output (for startup errors, etc.)
    pub fn add_warning(&mut self, message: String) {
        self.output.push_back(OutputLine {
            runner_label: String::new(),
            stream: crate::runner::StreamType::Status,
            content: message,
        });
    }

    // --- History recall ---

    /// Recall the most recent command from history into the input field
    pub fn recall_last_command(&mut self) {
        if !self.input.is_empty() {
            return;
        }
        if let Some(cmd) = self.searcher.most_recent_command() {
            self.input = cmd.command.clone();
            self.cursor_position = self.input.len();
            self.update_suggestions();
        }
    }

    // --- Output scrolling ---

    pub fn scroll_up(&mut self, lines: usize) {
        self.scroll_offset = self.scroll_offset.saturating_sub(lines);
        self.auto_scroll = false;
    }

    pub fn scroll_down(&mut self, lines: usize) {
        self.scroll_offset = self.scroll_offset.saturating_add(lines);
        // auto_scroll is re-enabled by the rendering logic when we're at the bottom
    }

    // --- Suggestions ---

    fn update_suggestions(&mut self) {
        self.suggestions = self
            .suggestion_engine
            .suggest(&self.input, &mut self.searcher, 8);
        self.selected_suggestion = 0;
    }

    pub fn accept_suggestion(&mut self) {
        if self.suggestions.is_empty() || self.selected_suggestion >= self.suggestions.len() {
            return;
        }

        let suggestion = &self.suggestions[self.selected_suggestion];

        match suggestion.suggestion_type {
            crate::suggest::SuggestionType::FullCommand => {
                self.input = suggestion.text.clone();
                self.cursor_position = self.input.len();
            }
            crate::suggest::SuggestionType::Argument
            | crate::suggest::SuggestionType::ArgumentValue => {
                if !self.input.ends_with(' ') {
                    let mut new_input = self.input.trim_end().to_string();
                    if let Some(last_space_pos) = new_input.rfind(char::is_whitespace) {
                        new_input.truncate(last_space_pos + 1);
                        new_input.push_str(&suggestion.text);
                    } else {
                        new_input = suggestion.text.clone();
                    }
                    self.input = new_input;
                } else {
                    self.input.push_str(&suggestion.text);
                }
                self.cursor_position = self.input.len();
            }
        }

        self.update_suggestions();
    }

    pub fn next_suggestion(&mut self) {
        if !self.suggestions.is_empty() {
            self.selected_suggestion = (self.selected_suggestion + 1) % self.suggestions.len();
        }
    }

    pub fn prev_suggestion(&mut self) {
        if !self.suggestions.is_empty() {
            if self.selected_suggestion == 0 {
                self.selected_suggestion = self.suggestions.len() - 1;
            } else {
                self.selected_suggestion -= 1;
            }
        }
    }

    pub fn get_suggestion_preview(&self) -> Option<String> {
        if self.suggestions.is_empty() || self.selected_suggestion >= self.suggestions.len() {
            return None;
        }

        let suggestion = &self.suggestions[self.selected_suggestion];

        match suggestion.suggestion_type {
            crate::suggest::SuggestionType::FullCommand => {
                if suggestion.text.starts_with(&self.input) {
                    Some(suggestion.text.get(self.input.len()..)
                        .unwrap_or("").to_string())
                } else if self.input.is_empty() {
                    Some(suggestion.text.clone())
                } else {
                    None
                }
            }
            crate::suggest::SuggestionType::Argument
            | crate::suggest::SuggestionType::ArgumentValue => {
                if self.input.ends_with(' ') {
                    Some(suggestion.text.clone())
                } else {
                    let last_word_start = self
                        .input
                        .rfind(char::is_whitespace)
                        .map(|pos| pos + self.input.get(pos..).and_then(|s| s.chars().next()).map_or(1, |c| c.len_utf8()))
                        .unwrap_or(0);
                    let current_word = self.input.get(last_word_start..).unwrap_or("");
                    if suggestion.text.starts_with(current_word) {
                        Some(suggestion.text.get(current_word.len()..)
                            .unwrap_or("").to_string())
                    } else {
                        Some(format!(" {}", suggestion.text))
                    }
                }
            }
        }
    }

    /// Compute the full resulting command for a suggestion, split into
    /// (already_typed_prefix, new_suggestion_suffix) for display purposes.
    pub fn suggestion_full_preview(&self, suggestion: &Suggestion) -> (String, String) {
        match suggestion.suggestion_type {
            crate::suggest::SuggestionType::FullCommand => {
                // The suggestion IS the full command
                if suggestion.text.starts_with(&self.input) && !self.input.is_empty() {
                    (
                        self.input.clone(),
                        suggestion.text.get(self.input.len()..)
                            .unwrap_or("").to_string(),
                    )
                } else {
                    (String::new(), suggestion.text.clone())
                }
            }
            crate::suggest::SuggestionType::Argument
            | crate::suggest::SuggestionType::ArgumentValue => {
                if !self.input.ends_with(' ') {
                    // Mid-word: the typed prefix is input up to the last space
                    let trimmed = self.input.trim_end();
                    if let Some(last_space) = trimmed.rfind(char::is_whitespace) {
                        let end = last_space + trimmed.get(last_space..).and_then(|s| s.chars().next()).map_or(1, |c| c.len_utf8());
                        let prefix = trimmed.get(..end).unwrap_or(trimmed);
                        (prefix.to_string(), suggestion.text.clone())
                    } else {
                        (String::new(), suggestion.text.clone())
                    }
                } else {
                    // Trailing space: typed prefix is the full input
                    (self.input.clone(), suggestion.text.clone())
                }
            }
        }
    }

    /// Build colorized spans for a full command suggestion.
    /// Tokens are classified as: typed prefix (dim gray), argument (cyan), value (green),
    /// or subcommand (bold white).
    pub fn colorize_command_suggestion<'a>(&self, suggestion: &Suggestion) -> Vec<Span<'a>> {
        let tokens: Vec<&str> = suggestion.text.split_whitespace().collect();
        let input_trimmed = self.input.trim_start();

        // Count how many leading tokens match what's already typed
        let input_tokens: Vec<&str> = input_trimmed.split_whitespace().collect();
        let typed_count = input_tokens
            .iter()
            .zip(tokens.iter())
            .take_while(|(a, b)| a == b)
            .count();

        // Find where the prefix ends (first '-' token)
        let prefix_end = tokens
            .iter()
            .position(|t| t.starts_with('-'))
            .unwrap_or(tokens.len());

        let mut spans = Vec::new();

        for (i, tok) in tokens.iter().enumerate() {
            if i > 0 {
                spans.push(Span::raw(" "));
            }

            let style = if i < typed_count {
                // Already typed — dim
                Style::default().fg(Color::DarkGray)
            } else if i < prefix_end {
                // Subcommand token (not yet typed)
                Style::default().fg(Color::White).add_modifier(Modifier::BOLD)
            } else if tok.starts_with('-') && *tok != "--" {
                // Argument token
                Style::default().fg(Color::Cyan)
            } else {
                // Value token
                Style::default().fg(Color::Green)
            };

            spans.push(Span::styled(tok.to_string(), style));
        }

        spans
    }

    // --- Quit ---

    /// Handle a quit key press (Ctrl+C, Ctrl+D, Esc). Returns true if should quit.
    pub fn try_quit(&mut self) -> bool {
        if let Some(last) = self.last_quit_press {
            if last.elapsed() < std::time::Duration::from_secs(1) {
                return true; // Second press within 1s — quit
            }
        }
        self.last_quit_press = Some(std::time::Instant::now());
        false
    }

    /// Whether the "press again to quit" hint should be shown
    pub fn is_quit_hint_active(&self) -> bool {
        self.last_quit_press
            .is_some_and(|t| t.elapsed() < std::time::Duration::from_secs(1))
    }
}

pub async fn run_tui(
    searcher: HistorySearcher,
    suggestion_engine: SuggestionEngine,
    startup_warnings: Vec<String>,
    config: Config,
) -> Result<HistorySearcher, Box<dyn std::error::Error>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let (output_tx, mut output_rx) = tokio::sync::mpsc::channel::<OutputMessage>(256);
    let mut runner = TaskRunner::new(output_tx, config.runner.max_concurrent);
    let mut event_stream = EventStream::new();

    let mut app = App::new(searcher, suggestion_engine, &config);
    for warning in startup_warnings {
        app.add_warning(warning);
    }
    let mut should_quit = false;
    let mut tick = tokio::time::interval(std::time::Duration::from_secs(1));

    loop {
        terminal.draw(|f| {
            let show_suggestions = app.has_suggestions();
            let box_pad_h = app.box_pad_h;
            let area = f.area();

            // Calculate input height: 1 line of content + 2 for borders, grows with wrapping
            let input_content_width = area.width.saturating_sub(2) as usize; // subtract border columns
            let input_lines = if input_content_width > 0 {
                (app.input().len() / input_content_width + 1).max(1) as u16
            } else {
                1
            };
            let input_height = input_lines + 2; // +2 for top/bottom border

            // Suggestions: 5 content lines + 2 borders when visible
            let suggestion_height: u16 = if show_suggestions { 7 } else { 0 };

            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Min(1),                       // Output: fills remaining
                    Constraint::Length(suggestion_height),     // Suggestions: 5 lines or 0
                    Constraint::Length(input_height),          // Input: adapts to content
                ])
                .split(area);

            // Output section
            let output_area_height = chunks[0].height.saturating_sub(2) as usize; // subtract borders
            let total_lines = app.output().len();

            // Clamp scroll_offset: it's the index of the first visible line (top-of-window).
            // Max value ensures the last page is fully visible.
            let max_scroll = total_lines.saturating_sub(output_area_height);
            let scroll_offset = app.scroll_offset().min(max_scroll);

            // Re-enable auto-scroll if we're at the bottom
            if scroll_offset >= max_scroll {
                app.auto_scroll = true;
            }
            // Update the stored offset to the clamped value
            app.scroll_offset = scroll_offset;

            let visible_start = scroll_offset;
            let visible_end = (scroll_offset + output_area_height).min(total_lines);

            let output_width = chunks[0].width.saturating_sub(2) as usize; // subtract borders

            let output_lines: Vec<Line> = app
                .output()
                .iter()
                .skip(visible_start)
                .take(visible_end - visible_start)
                .flat_map(|line| {
                    let border_style = Style::default().fg(Color::DarkGray);

                    // Box drawing for parallel output blocks
                    // 1 char inner padding on each side: │  content  │

                    if line.runner_label.starts_with("\x00top:") {
                        let label = &line.runner_label[5..];
                        let left = if label.is_empty() {
                            "┌".to_string()
                        } else {
                            format!("┌─ {} ", label)
                        };
                        let left_w = unicode_width::UnicodeWidthStr::width(left.as_str());
                        let right = "─┐";
                        let right_w = unicode_width::UnicodeWidthStr::width(right);
                        let fill_len = output_width.saturating_sub(left_w).saturating_sub(right_w);
                        let fill: String = "─".repeat(fill_len);

                        return vec![Line::from(vec![
                            Span::styled(left, border_style),
                            Span::styled(fill, border_style),
                            Span::styled(right, border_style),
                        ])];
                    }

                    if line.runner_label == "\x00bot" {
                        let left = "└";
                        let left_w = unicode_width::UnicodeWidthStr::width(left);

                        let (right, right_w) = if line.content.is_empty() {
                            ("─┘".to_string(), unicode_width::UnicodeWidthStr::width("─┘"))
                        } else {
                            let r = format!(" {} ─┘", line.content);
                            let w = unicode_width::UnicodeWidthStr::width(r.as_str());
                            (r, w)
                        };

                        let fill_len = output_width.saturating_sub(left_w).saturating_sub(right_w);
                        let fill: String = "─".repeat(fill_len);

                        return vec![Line::from(vec![
                            Span::styled(left, border_style),
                            Span::styled(fill, border_style),
                            Span::styled(right, border_style),
                        ])];
                    }

                    if line.runner_label == "\x00box" {
                        use ansi_to_tui::IntoText;
                        let parsed = line.content.as_bytes().into_text();
                        let content_lines = match parsed {
                            Ok(text) => text.lines,
                            Err(_) => vec![Line::from(line.content.clone())],
                        };

                        // Inner width: output_width minus "│" + pad on each side + "│"
                        let inner_width = output_width.saturating_sub(2 + box_pad_h * 2);
                        let h_pad = " ".repeat(box_pad_h);

                        return content_lines
                            .into_iter()
                            .map(|l| {
                                let content_width: usize = l.spans.iter().map(|s| {
                                    unicode_width::UnicodeWidthStr::width(s.content.as_ref())
                                }).sum();
                                let pad = inner_width.saturating_sub(content_width);

                                let mut spans = vec![
                                    Span::styled("│", border_style),
                                    Span::raw(h_pad.clone()),
                                ];
                                spans.extend(l.spans);
                                spans.push(Span::raw(" ".repeat(pad)));
                                spans.push(Span::raw(h_pad.clone()));
                                spans.push(Span::styled("│", border_style));
                                Line::from(spans)
                            })
                            .collect();
                    }

                    // Regular (non-parallel) rendering
                    match line.stream {
                        crate::runner::StreamType::Status => {
                            // Single command separator
                            let right = format!(" {} ", line.content);
                            let fill_len = output_width
                                .saturating_sub(1)
                                .saturating_sub(right.len());
                            let fill: String = "─".repeat(fill_len);

                            vec![Line::from(vec![
                                Span::raw(" "),
                                Span::styled(fill, Style::default().fg(Color::DarkGray)),
                                Span::styled(right, Style::default().fg(Color::DarkGray)),
                            ])]
                        }
                        crate::runner::StreamType::Output => {
                            use ansi_to_tui::IntoText;
                            let parsed = line.content.as_bytes().into_text();
                            match parsed {
                                Ok(text) => text.lines,
                                Err(_) => vec![Line::from(line.content.clone())],
                            }
                        }
                    }
                })
                .collect();

            let output_title = if let Some((completed, total)) = app.parallel_progress {
                if completed < total {
                    format!(" Output ({}/{} completed) ", completed, total)
                } else {
                    " Output ".to_string()
                }
            } else {
                " Output ".to_string()
            };

            let output = Paragraph::new(output_lines)
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title(output_title)
                        .border_style(Style::default().fg(Color::Cyan)),
                )
                .style(Style::default().fg(Color::White));
            f.render_widget(output, chunks[0]);

            // Suggestions section
            if show_suggestions {
                let items: Vec<ListItem> = app
                    .suggestions()
                    .iter()
                    .enumerate()
                    .map(|(i, suggestion)| {
                        let type_indicator = match suggestion.suggestion_type {
                            crate::suggest::SuggestionType::FullCommand => "cmd",
                            crate::suggest::SuggestionType::Argument => "arg",
                            crate::suggest::SuggestionType::ArgumentValue => "val",
                        };

                        let is_selected = i == app.selected_suggestion;
                        let indicator = if is_selected { "▌" } else { " " };

                        let mut spans = vec![
                            Span::styled(
                                indicator,
                                Style::default().fg(Color::Yellow),
                            ),
                            Span::styled(
                                format!("[{}] ", type_indicator),
                                Style::default().fg(Color::DarkGray),
                            ),
                        ];

                        if suggestion.suggestion_type == crate::suggest::SuggestionType::FullCommand {
                            spans.extend(app.colorize_command_suggestion(suggestion));
                        } else {
                            let (typed, new) = app.suggestion_full_preview(suggestion);
                            spans.push(Span::styled(typed, Style::default().fg(Color::DarkGray)));
                            spans.push(Span::styled(new, Style::default().fg(Color::Cyan)));
                        }

                        ListItem::new(Line::from(spans))
                    })
                    .collect();

                let suggestions_list = List::new(items)
                    .block(
                        Block::default()
                            .borders(Borders::ALL)
                            .title(" Suggestions (Tab/↑↓: navigate, →: next word, Ctrl+Y: accept) ")
                            .border_style(Style::default().fg(Color::Magenta)),
                    )
                    .style(Style::default().fg(Color::White));

                f.render_widget(suggestions_list, chunks[1]);
            }

            // Input section
            let input_text = if let Some(preview) = app.get_suggestion_preview() {
                let line = Line::from(vec![
                    Span::styled(app.input().to_string(), Style::default().fg(Color::White)),
                    Span::styled(
                        preview,
                        Style::default()
                            .fg(Color::DarkGray)
                            .add_modifier(Modifier::DIM),
                    ),
                ]);
                Text::from(line)
            } else {
                Text::from(app.input())
            };

            let (input_title, input_border_color) = if app.is_quit_hint_active() {
                (" Press Ctrl+C again to quit ", Color::Yellow)
            } else {
                (" Input ", Color::Green)
            };

            // Current time for the input border
            let now = chrono::Local::now();
            let time_str = now.format(" %H:%M:%S ").to_string();

            let input = Paragraph::new(input_text)
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title(input_title)
                        .title_bottom(
                            Line::from(time_str)
                                .right_aligned()
                                .style(Style::default().fg(Color::DarkGray))
                        )
                        .border_style(Style::default().fg(input_border_color)),
                )
                .style(Style::default().fg(Color::White));
            f.render_widget(input, chunks[2]);

            // Compute display width (not byte offset) for correct cursor placement
            let input = app.input();
            let pos = app.cursor_position().min(input.len());
            let display_col = input.get(..pos)
                .map(unicode_width::UnicodeWidthStr::width)
                .unwrap_or(0) as u16;
            f.set_cursor_position((
                chunks[2].x + display_col + 1,
                chunks[2].y + 1,
            ));
        })?;

        tokio::select! {
            Some(event_result) = event_stream.next() => {
                match event_result {
                    Ok(Event::Key(key)) => {
                        should_quit = keymap::handle_key_event(&mut app, key, &mut runner);
                    }
                    Ok(Event::Resize(cols, rows)) => {
                        runner.resize_all(cols, rows);
                    }
                    _ => {}
                }
            }
            Some(msg) = output_rx.recv() => {
                app.push_output(msg);
                // Drain all remaining messages before re-rendering
                while let Ok(msg) = output_rx.try_recv() {
                    app.push_output(msg);
                }
            }
            _ = tick.tick() => {
                // Forces a re-render to update the clock
            }
        }

        if should_quit {
            runner.cancel_all();
            break;
        }
    }

    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen
    )?;
    terminal.show_cursor()?;

    Ok(app.into_searcher())
}
