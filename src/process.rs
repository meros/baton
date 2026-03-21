use anyhow::Result;
use std::fs;
use std::path::Path;

pub struct ClaudeProcess {
    pub pid: u32,
    pub cwd: String,
}

/// Find all running Claude Code processes by scanning /proc.
pub fn find_claude_processes() -> Result<Vec<ClaudeProcess>> {
    let mut results = Vec::new();

    let proc_dir = Path::new("/proc");
    if !proc_dir.exists() {
        return Ok(results);
    }

    for entry in fs::read_dir(proc_dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy();

        // Only look at numeric dirs (PIDs)
        if !name_str.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }

        let pid: u32 = match name_str.parse() {
            Ok(p) => p,
            Err(_) => continue,
        };

        let pid_dir = entry.path();

        // Read the cmdline to check if it's a claude process
        let cmdline_path = pid_dir.join("cmdline");
        let cmdline = match fs::read(&cmdline_path) {
            Ok(data) => data,
            Err(_) => continue, // permission denied or gone
        };

        // cmdline is null-separated
        let cmdline_str = String::from_utf8_lossy(&cmdline);
        let args: Vec<&str> = cmdline_str.split('\0').collect();

        if !is_claude_process(&args) {
            continue;
        }

        // Read cwd symlink
        let cwd_path = pid_dir.join("cwd");
        let cwd = match fs::read_link(&cwd_path) {
            Ok(p) => p.to_string_lossy().to_string(),
            Err(_) => continue,
        };

        results.push(ClaudeProcess { pid, cwd });
    }

    Ok(results)
}

fn is_claude_process(args: &[&str]) -> bool {
    // Look for the claude binary in args
    // Typical: /nix/store/.../bin/.claude-unwrapped or /usr/local/bin/claude
    // Also: node .../claude/cli.js
    for arg in args {
        if arg.contains("claude") && !arg.contains("baton") {
            // Make sure it's actually the Claude Code CLI, not just any process
            // with "claude" in the path
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
