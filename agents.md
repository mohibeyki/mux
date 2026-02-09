# Agents Architecture

## Overview

The `mux` tool uses an agent-based architecture to run multiple CLI applications concurrently and merge their output into a single, unified stream. Each command runs in its own agent (worker), with output being collected and displayed in the TUI.

## Core Concepts

### Agent

An agent is an asynchronous worker responsible for:
- Spawning and managing a child process
- Capturing stdout/stderr streams
- Tagging output with metadata (command name, stream type, timestamp)
- Handling process lifecycle (start, monitor, terminate)
- Reporting process status and exit codes

### Agent Pool

The agent pool manages multiple agents concurrently:
- Creates and supervises agent instances
- Distributes commands to available agents
- Aggregates output from all agents
- Handles resource limits (max concurrent processes)

### Output Merger

The output merger combines streams from multiple agents:
- Preserves ordering where possible (timestamped)
- Tags output by source (which command produced it)
- Handles interleaved output gracefully
- Supports different merging strategies (line-buffered, immediate, etc.)

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                         TUI Layer                            │
│  ┌─────────────┐  ┌──────────────┐  ┌──────────────┐       │
│  │   Input     │  │   Output     │  │   Status     │       │
│  │   Panel     │  │   Panel      │  │   Panel      │       │
│  └─────────────┘  └──────────────┘  └──────────────┘       │
└──────────────────────┬──────────────────────────────────────┘
                       │
                       ▼
┌─────────────────────────────────────────────────────────────┐
│                      Agent Pool                              │
│  ┌────────────────────────────────────────────────────┐     │
│  │          Output Merger (mpsc channel)              │     │
│  └────────────────────────────────────────────────────┘     │
│         ▲              ▲              ▲                      │
│         │              │              │                      │
│  ┌──────┴───┐   ┌─────┴────┐   ┌────┴─────┐               │
│  │ Agent 1  │   │ Agent 2  │   │ Agent N  │               │
│  │ (Task)   │   │ (Task)   │   │ (Task)   │               │
│  └──────────┘   └──────────┘   └──────────┘               │
│       │              │              │                        │
└───────┼──────────────┼──────────────┼────────────────────────┘
        │              │              │
        ▼              ▼              ▼
   ┌────────┐     ┌────────┐     ┌────────┐
   │ Child  │     │ Child  │     │ Child  │
   │Process │     │Process │     │Process │
   │(cmd 1) │     │(cmd 2) │     │(cmd N) │
   └────────┘     └────────┘     └────────┘
```

## Component Details

### 1. Agent Implementation

```rust
struct Agent {
    id: AgentId,
    command: String,
    process: Child,
    stdout_stream: ChildStdout,
    stderr_stream: ChildStderr,
    status: AgentStatus,
}

enum AgentStatus {
    Starting,
    Running,
    Completed(ExitCode),
    Failed(Error),
    Terminated,
}
```

**Responsibilities:**
- Spawn command using `tokio::process::Command`
- Read stdout/stderr using `tokio::io::BufReader`
- Send output messages to the merger channel
- Monitor process health and exit status
- Handle graceful shutdown on termination signal

### 2. Agent Pool

```rust
struct AgentPool {
    agents: HashMap<AgentId, JoinHandle<Result<AgentStatus>>>,
    output_tx: mpsc::Sender<OutputMessage>,
    output_rx: mpsc::Receiver<OutputMessage>,
    max_concurrent: usize,
}
```

**Responsibilities:**
- Spawn new agents as tokio tasks
- Track active agent handles
- Enforce concurrency limits
- Provide channel for output aggregation
- Support graceful shutdown of all agents

### 3. Output Message Protocol

```rust
struct OutputMessage {
    agent_id: AgentId,
    timestamp: SystemTime,
    stream: StreamType,
    content: String,
}

enum StreamType {
    Stdout,
    Stderr,
    Status,  // Agent lifecycle events
}
```

### 4. Output Merger Strategies

#### Strategy: Interleaved (Default)
- Output appears as soon as it's produced
- Tagged with source (command name, color-coded)
- Minimal buffering for low latency

#### Strategy: Line-Buffered
- Complete lines only (no partial output)
- Preserves line integrity per command
- Slight delay for incomplete lines

#### Strategy: Grouped
- Group output by command
- Display each command's output in blocks
- Useful for non-interactive batch processing

## Message Flow

1. **User Input** → TUI receives command(s) to run
2. **Agent Creation** → AgentPool spawns Agent task for each command
3. **Process Spawn** → Agent spawns child process via tokio::process
4. **Output Capture** → Agent reads stdout/stderr asynchronously
5. **Message Dispatch** → Agent sends OutputMessage to merger channel
6. **Message Receipt** → AgentPool receives messages via mpsc::Receiver
7. **Display Update** → TUI polls for new messages and updates output panel

## Error Handling

### Process Failures
- Non-zero exit codes reported as warnings
- Process crash logged with error details
- Failed agents remain in pool with status for inspection

### Stream Errors
- Broken pipes handled gracefully
- EOF detection for clean termination
- Read errors logged without crashing agent

### Resource Exhaustion
- Max concurrent limit prevents process explosion
- Queue commands if limit reached
- Display pending command count in status

## Concurrency Model

Using Tokio for async execution:
- **One task per agent** for isolation
- **mpsc channel** for thread-safe message passing
- **Non-blocking I/O** for stdout/stderr reading
- **Structured concurrency** with JoinHandles for cleanup

## Integration with Shell History

The `history` module can be used to:
- Provide command suggestions from shell history
- Pre-fill common parallel command patterns
- Learn frequently used command combinations
- Support command templates (e.g., run tests in all subdirs)

## Future Enhancements

### Process Control
- Pause/resume individual agents
- Restart failed commands
- Send signals (SIGTERM, SIGKILL) to specific processes

### Advanced Output
- Search/filter output by command
- Export output to separate files per command
- Syntax highlighting for known output formats
- Collapsible output sections in TUI

### Performance Monitoring
- Track CPU/memory usage per agent
- Display execution time for each command
- Show throughput metrics (lines/sec, bytes/sec)

### Input Distribution
- Send stdin to specific agents
- Broadcast stdin to all agents
- Interactive mode for selecting target agent

## Configuration

Planned configuration options:
```toml
[agents]
max_concurrent = 10        # Maximum parallel processes
output_buffer_size = 1024  # Lines to keep in memory
merge_strategy = "interleaved"  # Output merging approach

[process]
default_timeout = 300      # Seconds before SIGTERM
kill_timeout = 10          # Seconds before SIGKILL after SIGTERM
working_directory = "."    # Default working dir for commands

[display]
color_per_agent = true     # Assign unique colors to agents
show_timestamps = false    # Prefix output with timestamps
show_exit_codes = true     # Display exit status on completion
```

## Implementation Phases

### Phase 1: Basic Agent Execution
- Spawn single command in agent
- Capture and display stdout
- Handle process exit

### Phase 2: Parallel Execution
- Multiple agents running concurrently
- Output merging with tagging
- Status tracking per agent

### Phase 3: Advanced Control
- Interactive process control
- Signal handling
- Resource monitoring

### Phase 4: Polish & Features
- Shell history integration
- Configuration file support
- Advanced TUI features (filtering, search)
