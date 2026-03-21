use anyhow::Result;
use std::collections::HashMap;
use std::fs;
use std::path::Path;

pub struct ClaudeProcess {
    pub pid: u32,
    pub cwd: String,
    pub cpu_ticks: u64,
    pub child_commands: Vec<String>,
}

/// Find all running Claude Code processes by scanning /proc.
pub fn find_claude_processes() -> Result<Vec<ClaudeProcess>> {
    let mut results = Vec::new();

    let proc_dir = Path::new("/proc");
    if !proc_dir.exists() {
        return Ok(results);
    }

    // First pass: find claude processes
    let mut claude_pids: Vec<(u32, String)> = Vec::new();

    for entry in fs::read_dir(proc_dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy();

        if !name_str.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }

        let pid: u32 = match name_str.parse() {
            Ok(p) => p,
            Err(_) => continue,
        };

        let pid_dir = entry.path();

        let cmdline = match fs::read(pid_dir.join("cmdline")) {
            Ok(data) => data,
            Err(_) => continue,
        };

        let cmdline_str = String::from_utf8_lossy(&cmdline);
        let args: Vec<&str> = cmdline_str.split('\0').collect();

        if !is_claude_process(&args) {
            continue;
        }

        let cwd = match fs::read_link(pid_dir.join("cwd")) {
            Ok(p) => p.to_string_lossy().to_string(),
            Err(_) => continue,
        };

        claude_pids.push((pid, cwd));
    }

    // Second pass: for each claude process, get CPU ticks and find child commands
    for (pid, cwd) in claude_pids {
        let cpu_ticks = read_cpu_ticks(pid);
        let child_commands = find_child_commands(pid);

        results.push(ClaudeProcess {
            pid,
            cwd,
            cpu_ticks,
            child_commands,
        });
    }

    Ok(results)
}

/// Read utime + stime from /proc/<pid>/stat
pub fn read_cpu_ticks(pid: u32) -> u64 {
    let stat = match fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(s) => s,
        Err(_) => return 0,
    };

    // Format: pid (comm) state ppid pgrp session tty_nr tpgid flags
    //         minflt cminflt majflt cmajflt utime stime ...
    //         Field indices (0-based after comm): utime=11, stime=12
    let after_comm = match stat.rsplit_once(')') {
        Some((_, rest)) => rest,
        None => return 0,
    };

    let fields: Vec<&str> = after_comm.split_whitespace().collect();
    // fields[0] = state, fields[1] = ppid, ..., fields[11] = utime, fields[12] = stime
    if fields.len() < 13 {
        return 0;
    }

    let utime: u64 = fields[11].parse().unwrap_or(0);
    let stime: u64 = fields[12].parse().unwrap_or(0);
    utime + stime
}

/// Find interesting child process commands (tools claude is running)
fn find_child_commands(parent_pid: u32) -> Vec<String> {
    let mut commands = Vec::new();

    // Build a quick parent→children map by scanning /proc
    let children = find_descendant_pids(parent_pid);

    for child_pid in children {
        let cmdline_path = format!("/proc/{child_pid}/cmdline");
        let cmdline = match fs::read(&cmdline_path) {
            Ok(data) => data,
            Err(_) => continue,
        };

        let cmdline_str = String::from_utf8_lossy(&cmdline);
        let args: Vec<&str> = cmdline_str.split('\0').filter(|s| !s.is_empty()).collect();
        if args.is_empty() {
            continue;
        }

        // Extract the base command name
        let cmd = Path::new(args[0])
            .file_name()
            .map(|f| f.to_string_lossy().to_string())
            .unwrap_or_default();

        // Only report interesting commands (not node/claude internals)
        if is_interesting_child(&cmd) && !is_claude_internal(&args) {
            // For node/npx, show the script name instead of "node"
            let summary = if (cmd == "node" || cmd == "npx") && args.len() > 1 {
                let script = Path::new(args[1])
                    .file_name()
                    .map(|f| f.to_string_lossy().to_string())
                    .unwrap_or_else(|| args[1].to_string());
                if args.len() > 2 {
                    format!("{} {}", script, args[2..].join(" "))
                } else {
                    script
                }
            } else if args.len() > 1 {
                format!("{} {}", cmd, args[1..].join(" "))
            } else {
                cmd
            };
            // Truncate long commands
            let summary = if summary.chars().count() > 60 {
                let t: String = summary.chars().take(59).collect();
                format!("{t}…")
            } else {
                summary
            };
            commands.push(summary);
        }
    }

    commands
}

/// Find all descendant PIDs of a process
fn find_descendant_pids(root_pid: u32) -> Vec<u32> {
    // Build ppid→children map
    let mut ppid_map: HashMap<u32, Vec<u32>> = HashMap::new();

    let proc_dir = Path::new("/proc");
    if let Ok(entries) = fs::read_dir(proc_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if let Ok(pid) = name_str.parse::<u32>() {
                if let Some(ppid) = read_ppid(pid) {
                    ppid_map.entry(ppid).or_default().push(pid);
                }
            }
        }
    }

    // BFS from root_pid
    let mut result = Vec::new();
    let mut queue = vec![root_pid];
    while let Some(pid) = queue.pop() {
        if let Some(children) = ppid_map.get(&pid) {
            for &child in children {
                result.push(child);
                queue.push(child);
            }
        }
    }

    result
}

fn read_ppid(pid: u32) -> Option<u32> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let after_comm = stat.rsplit_once(')')?.1;
    let mut fields = after_comm.split_whitespace();
    fields.next()?; // state
    fields.next()?.parse().ok() // ppid
}

fn is_interesting_child(cmd: &str) -> bool {
    matches!(
        cmd,
        "jest"
            | "vitest"
            | "cargo"
            | "rustc"
            | "tsc"
            | "node"
            | "npx"
            | "npm"
            | "pnpm"
            | "python"
            | "python3"
            | "pytest"
            | "go"
            | "make"
            | "git"
            | "gcc"
            | "g++"
            | "clang"
            | "sh"
            | "bash"
            | "zsh"
    )
}

/// Check if this child process is infrastructure (not a tool claude spawned for a task)
fn is_claude_internal(args: &[&str]) -> bool {
    let full = args.join(" ");
    full.contains("claude")
        || full.contains("@anthropic-ai")
        || full.contains("claude-code")
        || full.contains(".claude-unwrapped")
        // MCP servers — persistent background processes, not tool invocations
        || full.contains("mcp")
        || full.contains("language-server")
        || full.contains("lsp")
        || full.contains("tsserver")
        || full.contains("gopls")
        || full.contains("rust-analyzer")
        || full.contains("context7")
        || full.contains("server.js")
}

fn is_claude_process(args: &[&str]) -> bool {
    for arg in args {
        if arg.contains("claude") && !arg.contains("baton") {
            if arg.ends_with("/claude")
                || arg.ends_with("/.claude-unwrapped")
                || arg.contains("/claude-code/")
                || arg.contains("/@anthropic-ai/claude-code/")
            {
                return true;
            }
        }
    }
    false
}
