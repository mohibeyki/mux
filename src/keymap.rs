use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use crate::runner::TaskRunner;
use crate::tui::App;

/// Handle keyboard input for the application
/// Returns true if the application should quit
pub fn handle_key_event(app: &mut App, key: KeyEvent, runner: &mut TaskRunner) -> bool {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);

    match key.code {
        // Quit (double-press Ctrl+C or Ctrl+D within 1s)
        KeyCode::Char('c') if ctrl => return app.try_quit(),
        KeyCode::Char('d') if ctrl => {
            if app.input().is_empty() {
                return app.try_quit();
            } else {
                app.delete_char_forward();
            }
        }
        KeyCode::Esc => return app.try_quit(),

        // Suggestions
        KeyCode::Tab => app.next_suggestion(),
        KeyCode::BackTab => app.prev_suggestion(),
        KeyCode::Char('y') if ctrl => app.accept_suggestion(),
        KeyCode::Char('p') if ctrl => app.prev_suggestion(),
        KeyCode::Char('n') if ctrl => app.next_suggestion(),
        KeyCode::Up => {
            if app.input().is_empty() {
                app.recall_last_command();
            } else {
                app.prev_suggestion();
            }
        }
        KeyCode::Down => app.next_suggestion(),

        // Text input
        KeyCode::Char(c) if !ctrl && !alt => app.insert_char(c),

        // Line editing (emacs-style)
        KeyCode::Char('a') if ctrl => app.move_cursor_home(),
        KeyCode::Char('e') if ctrl => app.move_cursor_end(),
        KeyCode::Char('w') if ctrl => app.delete_word_backward(),
        KeyCode::Char('u') if ctrl => app.delete_to_line_start(),
        KeyCode::Char('k') if ctrl => app.delete_to_line_end(),
        KeyCode::Char('l') if ctrl => app.clear_output(),

        // Delete operations
        KeyCode::Backspace if alt => app.delete_word_backward(),
        KeyCode::Backspace => app.delete_char_backward(),
        KeyCode::Char('d') if alt => app.delete_word_forward(),
        KeyCode::Delete if alt => app.delete_word_forward(),
        KeyCode::Delete => app.delete_char_forward(),

        // Cursor movement
        KeyCode::Char('b') if ctrl => app.move_cursor_left(),
        KeyCode::Char('f') if ctrl => app.move_cursor_right(),
        KeyCode::Char('b') if alt => app.move_cursor_word_left(),
        KeyCode::Char('f') if alt => app.move_cursor_word_right(),
        KeyCode::Left if alt | ctrl => app.move_cursor_word_left(),
        KeyCode::Left => app.move_cursor_left(),
        KeyCode::Right if alt | ctrl => app.move_cursor_word_right(),
        KeyCode::Right => {
            if app.cursor_position() == app.input().len() {
                app.accept_next_preview_word();
            } else {
                app.move_cursor_right();
            }
        }
        KeyCode::Home => app.move_cursor_home(),
        KeyCode::End => app.move_cursor_end(),

        // Output scrolling
        KeyCode::PageUp => app.scroll_up(10),
        KeyCode::PageDown => app.scroll_down(10),

        // Submit
        KeyCode::Enter => return app.submit_command(runner),

        _ => {}
    }
    false
}
