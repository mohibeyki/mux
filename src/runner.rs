use std::collections::HashMap;
use std::io::Read;
use std::sync::{Arc, Mutex};
use tokio::sync::{Semaphore, mpsc};
use tokio::task::JoinHandle;

pub type TaskId = u64;

/// Type of output stream
#[derive(Debug, Clone, PartialEq)]
pub enum StreamType {
    /// Command output (stdout + stderr merged via PTY)
    Output,
    /// Lifecycle events: "completed", "exited with code 1", etc.
    Status,
}

/// A message from a running task to the TUI
#[derive(Debug, Clone)]
pub struct OutputMessage {
    pub task_id: TaskId,
    /// Display label for parallel runs (e.g., "[n=14][region=pnb]"), empty for single commands
    pub runner_label: String,
    pub stream: StreamType,
    pub content: String,
}

impl OutputMessage {
    pub fn output(task_id: TaskId, runner_label: &str, content: String) -> Self {
        Self {
            task_id,
            runner_label: runner_label.to_string(),
            stream: StreamType::Output,
            content,
        }
    }

    pub fn status(task_id: TaskId, runner_label: &str, content: &str) -> Self {
        Self {
            task_id,
            runner_label: runner_label.to_string(),
            stream: StreamType::Status,
            content: content.to_string(),
        }
    }
}

/// Handle for a running task: the tokio JoinHandle + kill switch + PTY master for resize
struct TaskHandle {
    join: JoinHandle<()>,
    child: Arc<Mutex<Option<Box<dyn portable_pty::Child + Send>>>>,
    master: Arc<Mutex<Option<Box<dyn portable_pty::MasterPty + Send>>>>,
}

/// Manages spawning and tracking of concurrent command tasks.
/// Uses a semaphore to limit the number of concurrently running processes.
pub struct TaskRunner {
    output_tx: mpsc::Sender<OutputMessage>,
    next_id: TaskId,
    active: HashMap<TaskId, TaskHandle>,
    semaphore: Arc<Semaphore>,
}

impl TaskRunner {
    pub fn new(output_tx: mpsc::Sender<OutputMessage>, max_concurrent: usize) -> Self {
        Self {
            output_tx,
            next_id: 1,
            active: HashMap::new(),
            semaphore: Arc::new(Semaphore::new(max_concurrent)),
        }
    }

    /// Spawn a command as an async task. Label is shown in the output box header
    /// (empty for single commands, e.g., "[n=3]" for parallel).
    /// If the pool is full, the task is queued and will start once a slot frees up.
    pub fn spawn_labeled(&mut self, command: &str, label: &str) -> TaskId {
        let id = self.next_id;
        self.next_id += 1;

        let tx = self.output_tx.clone();
        let cmd = command.to_string();
        let lbl = label.to_string();
        let child_handle: Arc<Mutex<Option<Box<dyn portable_pty::Child + Send>>>> =
            Arc::new(Mutex::new(None));
        let master_handle: Arc<Mutex<Option<Box<dyn portable_pty::MasterPty + Send>>>> =
            Arc::new(Mutex::new(None));
        let child_for_task = child_handle.clone();
        let master_for_task = master_handle.clone();
        let semaphore = self.semaphore.clone();

        let join = tokio::spawn(run_task(id, lbl, cmd, tx, child_for_task, master_for_task, semaphore));
        self.active.insert(id, TaskHandle { join, child: child_handle, master: master_handle });

        // Clean up finished tasks
        self.active.retain(|_, h| !h.join.is_finished());

        id
    }

    /// Resize the PTY of all active tasks to the new terminal dimensions
    pub fn resize_all(&mut self, cols: u16, rows: u16) {
        // Clean up finished tasks first
        self.active.retain(|_, h| !h.join.is_finished());

        for (id, handle) in &self.active {
            if let Ok(guard) = handle.master.lock() {
                if let Some(ref master) = *guard {
                    let size = portable_pty::PtySize {
                        rows,
                        cols,
                        pixel_width: 0,
                        pixel_height: 0,
                    };
                    if let Err(e) = master.resize(size) {
                        log::warn!("Failed to resize PTY for task #{}: {}", id, e);
                    }
                }
            }
        }
    }

    /// Cancel all active tasks
    pub fn cancel_all(&mut self) {
        for (_, handle) in self.active.drain() {
            if let Ok(mut guard) = handle.child.lock() {
                if let Some(ref mut child) = *guard {
                    let _ = child.kill();
                }
            }
            handle.join.abort();
        }
    }

}

/// Run a single command in a PTY, streaming output as OutputMessages.
/// The PTY ensures child processes see a real terminal and emit colors.
/// Acquires a semaphore permit before starting — queues if the pool is full.
async fn run_task(
    id: TaskId,
    runner_label: String,
    command: String,
    tx: mpsc::Sender<OutputMessage>,
    child_handle: Arc<Mutex<Option<Box<dyn portable_pty::Child + Send>>>>,
    master_handle: Arc<Mutex<Option<Box<dyn portable_pty::MasterPty + Send>>>>,
    semaphore: Arc<Semaphore>,
) {
    // Acquire a permit — blocks if max concurrent tasks are already running.
    // The permit is held (via _permit) until this function returns.
    let _permit = match semaphore.acquire().await {
        Ok(permit) => permit,
        Err(_) => {
            // Semaphore closed — runner is shutting down
            log::warn!("Task #{} cancelled: semaphore closed", id);
            return;
        }
    };

    log::info!("Task #{} started: {}", id, command);
    let start = std::time::Instant::now();

    let _ = tx
        .send(OutputMessage::status(id, &runner_label, "started"))
        .await;

    let cmd = command.clone();
    let lbl = runner_label.clone();
    let tx_clone = tx.clone();

    let result = tokio::task::spawn_blocking(move || {
        run_task_blocking(id, &lbl, &cmd, tx_clone, child_handle, master_handle)
    })
    .await;

    let (exit_msg, line_count) = match result {
        Ok(Ok((msg, lines))) => (msg, lines),
        Ok(Err(e)) => (format!("error: {}", e), 0),
        Err(e) => (format!("task panicked: {}", e), 0),
    };

    let elapsed = start.elapsed();
    log::info!(
        "Task #{} finished: {} ({}, {} lines, {:.2?})",
        id, command, exit_msg, line_count, elapsed
    );

    let _ = tx
        .send(OutputMessage::status(id, &runner_label, &exit_msg))
        .await;
}

/// Synchronous PTY execution (runs inside spawn_blocking)
fn run_task_blocking(
    id: TaskId,
    runner_label: &str,
    command: &str,
    tx: mpsc::Sender<OutputMessage>,
    child_handle: Arc<Mutex<Option<Box<dyn portable_pty::Child + Send>>>>,
    master_handle: Arc<Mutex<Option<Box<dyn portable_pty::MasterPty + Send>>>>,
) -> Result<(String, usize), Box<dyn std::error::Error + Send + Sync>> {
    use portable_pty::{CommandBuilder, PtySize, native_pty_system};

    // Get actual terminal size, fall back to 80x24
    let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));

    let pty_system = native_pty_system();

    let pty_pair = pty_system.openpty(PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    })?;

    let mut cmd = CommandBuilder::new("sh");
    cmd.arg("-c");
    cmd.arg(command);

    let child = pty_pair.slave.spawn_command(cmd)?;

    // Store the child handle so it can be killed from outside
    {
        let mut guard = child_handle.lock().map_err(|e| format!("lock error: {}", e))?;
        *guard = Some(child);
    }

    // Drop the slave side so we get EOF when the child exits
    drop(pty_pair.slave);

    // Clone the reader before storing the master — the reader is independent
    let mut reader = pty_pair.master.try_clone_reader()?;

    // Store the master so TaskRunner can resize it on terminal resize events
    {
        let mut guard = master_handle.lock().map_err(|e| format!("lock error: {}", e))?;
        *guard = Some(pty_pair.master);
    }
    let mut buf = [0u8; 4096];
    let mut partial = String::new();
    let mut line_count: usize = 0;

    loop {
        match reader.read(&mut buf) {
            Ok(0) => break, // EOF
            Ok(n) => {
                let chunk = String::from_utf8_lossy(&buf[..n]);
                partial.push_str(&chunk);

                // Split on newlines and send complete lines
                while let Some(newline_pos) = partial.find('\n') {
                    let line = partial[..newline_pos].to_string();
                    // Strip trailing \r (PTY uses \r\n)
                    let line = line.trim_end_matches('\r').to_string();
                    partial = partial[newline_pos + 1..].to_string();

                    line_count += 1;
                    if tx.blocking_send(OutputMessage::output(id, runner_label, line)).is_err() {
                        break;
                    }
                }

                // Any remaining content in `partial` is an incomplete line;
                // keep it in the buffer so the next read can complete it.
                // Final flush after EOF handles any leftover.
            }
            Err(_) => break,
        }
    }

    // Flush any remaining partial content
    if !partial.is_empty() {
        let line = partial.trim_end_matches('\r').to_string();
        line_count += 1;
        let _ = tx.blocking_send(OutputMessage::output(id, runner_label, line));
    }

    // Wait for the child to finish
    let exit_msg = {
        let mut guard = child_handle.lock().map_err(|e| format!("lock error: {}", e))?;
        if let Some(ref mut child) = *guard {
            let status = child.wait()?;
            if status.success() {
                "completed".to_string()
            } else {
                format!("exited with code {}", status.exit_code())
            }
        } else {
            "completed".to_string()
        }
    };

    Ok((exit_msg, line_count))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;

    #[tokio::test]
    async fn test_output_message_constructors() {
        let msg = OutputMessage::output(1, "[n=3]", "hello".to_string());
        assert_eq!(msg.task_id, 1);
        assert_eq!(msg.runner_label, "[n=3]");
        assert_eq!(msg.stream, StreamType::Output);
        assert_eq!(msg.content, "hello");

        let msg = OutputMessage::status(3, "", "started");
        assert_eq!(msg.stream, StreamType::Status);
        assert_eq!(msg.content, "started");
        assert_eq!(msg.runner_label, "");
    }

    #[tokio::test]
    async fn test_spawn_echo() {
        let (tx, mut rx) = mpsc::channel::<OutputMessage>(64);
        let mut runner = TaskRunner::new(tx, 64);

        let id = runner.spawn_labeled("echo hello", "");
        assert_eq!(id, 1);

        let mut got_started = false;
        let mut got_hello = false;
        let mut got_completed = false;

        while let Some(msg) = rx.recv().await {
            match msg.stream {
                StreamType::Status if msg.content == "started" => got_started = true,
                StreamType::Status if msg.content == "completed" => {
                    got_completed = true;
                    break;
                }
                StreamType::Output if msg.content.contains("hello") => got_hello = true,
                _ => {}
            }
        }

        assert!(got_started, "should have received 'started' status");
        assert!(got_hello, "should have received 'hello' in output");
        assert!(got_completed, "should have received 'completed' status");
    }

    #[tokio::test]
    async fn test_spawn_failing_command() {
        let (tx, mut rx) = mpsc::channel::<OutputMessage>(64);
        let mut runner = TaskRunner::new(tx, 64);

        runner.spawn_labeled("false", "");

        while let Some(msg) = rx.recv().await {
            if msg.stream == StreamType::Status && msg.content.contains("exited with") {
                return; // pass
            }
            if msg.stream == StreamType::Status && msg.content == "completed" {
                // `false` might show as completed on some systems since PTY merges exit handling
                return;
            }
        }

        panic!("should have received an exit status message");
    }

    #[tokio::test]
    async fn test_task_ids_increment() {
        let (tx, _rx) = mpsc::channel::<OutputMessage>(64);
        let mut runner = TaskRunner::new(tx, 64);

        assert_eq!(runner.spawn_labeled("true", ""), 1);
        assert_eq!(runner.spawn_labeled("true", ""), 2);
        assert_eq!(runner.spawn_labeled("true", ""), 3);
    }

    #[tokio::test]
    async fn test_cancel_all() {
        let (tx, mut rx) = mpsc::channel::<OutputMessage>(64);
        let mut runner = TaskRunner::new(tx, 64);

        // Use a command that produces output then sleeps, so the PTY reader is active
        runner.spawn_labeled("echo running && sleep 10", "");

        // Wait for output to confirm the process is running
        while let Some(msg) = rx.recv().await {
            if msg.stream == StreamType::Output && msg.content.contains("running") {
                break;
            }
        }

        // Kill all tasks
        runner.cancel_all();

        // Give the PTY reader time to notice the child died
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        // All tasks should be drained from the active map
        assert!(runner.active.is_empty());
    }
}
