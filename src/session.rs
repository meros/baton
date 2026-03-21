use anyhow::Result;
use chrono::{DateTime, Utc};
use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::Path;

pub struct SessionData {
    pub session_id: Option<String>,
    pub branch: Option<String>,
    pub last_activity: DateTime<Utc>,
    pub last_doing: Option<String>,
    pub repeated_edits: usize,
    pub activity_log: Vec<ActivityEntry>,
}

pub struct ActivityEntry {
    pub time: DateTime<Utc>,
    pub kind: String,
    pub summary: String,
}

/// Find the latest session JSONL file that matches a given cwd.
///
/// Claude Code stores sessions in ~/.claude/projects/<path-hash>/<session-id>.jsonl
/// The path-hash is the cwd with / replaced by - and leading - stripped.
pub fn find_latest_session(claude_projects: &Path, cwd: &str) -> Option<SessionData> {
    // Convert cwd to the project dir name format Claude uses
    let project_dir_name = cwd_to_project_dir(cwd);
    let project_dir = claude_projects.join(&project_dir_name);

    if !project_dir.exists() {
        return None;
    }

    // Find the most recently modified .jsonl file (excluding agent- prefixed ones)
    let mut jsonl_files: Vec<_> = fs::read_dir(&project_dir)
        .ok()?
        .filter_map(|e| e.ok())
        .filter(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            name.ends_with(".jsonl") && !name.starts_with("agent-")
        })
        .collect();

    jsonl_files.sort_by_key(|e| {
        e.metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH)
    });

    let latest = jsonl_files.last()?;
    parse_session_file(&latest.path())
}

/// Convert a cwd like "/home/meros/git/personal/baton" to
/// the project directory name "-home-meros-git-personal-baton"
fn cwd_to_project_dir(cwd: &str) -> String {
    cwd.replace('/', "-")
}

/// Parse a session JSONL file, reading from the tail for efficiency.
fn parse_session_file(path: &Path) -> Option<SessionData> {
    // Read the last ~100KB to get recent activity without reading the whole file
    let tail_lines = read_tail_lines(path, 100 * 1024).ok()?;

    let mut last_activity = DateTime::<Utc>::MIN_UTC;
    let mut last_doing: Option<String> = None;
    let mut session_id: Option<String> = None;
    let mut branch: Option<String> = None;
    let mut activity_log: Vec<ActivityEntry> = Vec::new();
    let mut recent_edit_files: Vec<String> = Vec::new();

    for line in &tail_lines {
        let v: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        // Extract timestamp
        let timestamp = v
            .get("timestamp")
            .and_then(|t| t.as_str())
            .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
            .map(|t| t.with_timezone(&Utc));

        if let Some(ts) = timestamp {
            if ts > last_activity {
                last_activity = ts;
            }
        }

        // Extract session metadata
        if session_id.is_none() {
            if let Some(sid) = v.get("sessionId").and_then(|s| s.as_str()) {
                session_id = Some(sid.to_string());
            }
        }

        if let Some(b) = v.get("gitBranch").and_then(|s| s.as_str()) {
            branch = Some(b.to_string());
        }

        let msg_type = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
        let ts = timestamp.unwrap_or(DateTime::<Utc>::MIN_UTC);

        match msg_type {
            "assistant" => {
                if let Some(content) = extract_assistant_content(&v) {
                    // Check for tool use
                    if let Some(tool_info) = extract_tool_use(&v) {
                        last_doing = Some(tool_info.clone());
                        activity_log.push(ActivityEntry {
                            time: ts,
                            kind: "tool_use".to_string(),
                            summary: tool_info.clone(),
                        });

                        // Track file edits for stuck detection
                        if let Some(file) = extract_edit_file(&v) {
                            recent_edit_files.push(file);
                        }
                    } else if !content.is_empty() {
                        // Text response — summarize
                        let summary = first_line(&content, 80);
                        last_doing = Some(summary.clone());
                        activity_log.push(ActivityEntry {
                            time: ts,
                            kind: "assistant".to_string(),
                            summary,
                        });
                    }
                }
            }
            "user" => {
                if let Some(text) = extract_user_text(&v) {
                    activity_log.push(ActivityEntry {
                        time: ts,
                        kind: "user".to_string(),
                        summary: first_line(&text, 80),
                    });
                }
            }
            _ => {}
        }
    }

    // Count repeated edits to the same file in the last N entries
    let repeated_edits = count_repeated_edits(&recent_edit_files);

    Some(SessionData {
        session_id,
        branch,
        last_activity,
        last_doing,
        repeated_edits,
        activity_log,
    })
}

fn extract_assistant_content(v: &Value) -> Option<String> {
    let message = v.get("message")?;
    let content = message.get("content")?;

    if let Some(text) = content.as_str() {
        return Some(text.to_string());
    }

    // Content is an array of blocks
    if let Some(arr) = content.as_array() {
        let mut texts = Vec::new();
        for block in arr {
            if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                texts.push(text.to_string());
            }
        }
        if !texts.is_empty() {
            return Some(texts.join(" "));
        }
    }

    None
}

fn extract_tool_use(v: &Value) -> Option<String> {
    let message = v.get("message")?;
    let content = message.get("content")?.as_array()?;

    for block in content {
        if block.get("type").and_then(|t| t.as_str()) == Some("tool_use") {
            let tool_name = block.get("name").and_then(|n| n.as_str()).unwrap_or("?");
            let input = block.get("input");

            let detail = match tool_name {
                "Edit" | "Write" | "Read" => input
                    .and_then(|i| i.get("file_path"))
                    .and_then(|p| p.as_str())
                    .map(|p| shorten_file_path(p))
                    .unwrap_or_default(),
                "Bash" => input
                    .and_then(|i| i.get("command"))
                    .and_then(|c| c.as_str())
                    .map(|c| first_line(c, 40))
                    .unwrap_or_default(),
                "Grep" | "Glob" => input
                    .and_then(|i| i.get("pattern"))
                    .and_then(|p| p.as_str())
                    .unwrap_or("")
                    .to_string(),
                _ => String::new(),
            };

            if detail.is_empty() {
                return Some(tool_name.to_string());
            }
            return Some(format!("{tool_name}: {detail}"));
        }
    }

    None
}

fn extract_edit_file(v: &Value) -> Option<String> {
    let message = v.get("message")?;
    let content = message.get("content")?.as_array()?;

    for block in content {
        let tool_name = block.get("name").and_then(|n| n.as_str())?;
        if tool_name == "Edit" || tool_name == "Write" {
            return block
                .get("input")
                .and_then(|i| i.get("file_path"))
                .and_then(|p| p.as_str())
                .map(|s| s.to_string());
        }
    }

    None
}

fn extract_user_text(v: &Value) -> Option<String> {
    let message = v.get("message")?;
    let content = message.get("content")?;

    if let Some(text) = content.as_str() {
        return Some(text.to_string());
    }

    if let Some(arr) = content.as_array() {
        for block in arr {
            if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                return Some(text.to_string());
            }
        }
    }

    None
}

fn count_repeated_edits(files: &[String]) -> usize {
    if files.len() < 3 {
        return 0;
    }
    // Look at last 10 edits
    let recent = if files.len() > 10 {
        &files[files.len() - 10..]
    } else {
        files
    };

    let mut counts: HashMap<&str, usize> = HashMap::new();
    for f in recent {
        *counts.entry(f.as_str()).or_default() += 1;
    }

    counts.values().copied().max().unwrap_or(0)
}

/// Read the last `bytes` of a file and return lines.
fn read_tail_lines(path: &Path, bytes: u64) -> Result<Vec<String>> {
    let mut file = fs::File::open(path)?;
    let metadata = file.metadata()?;
    let file_size = metadata.len();

    if file_size > bytes {
        file.seek(SeekFrom::End(-(bytes as i64)))?;
        // Skip partial first line
        let mut buf = [0u8; 1];
        loop {
            if file.read(&mut buf)? == 0 {
                break;
            }
            if buf[0] == b'\n' {
                break;
            }
        }
    }

    let reader = BufReader::new(file);
    let lines: Vec<String> = reader.lines().filter_map(|l| l.ok()).collect();
    Ok(lines)
}

fn shorten_file_path(path: &str) -> String {
    // Show just filename or last 2 components
    let parts: Vec<&str> = path.split('/').collect();
    if parts.len() <= 2 {
        path.to_string()
    } else {
        parts[parts.len() - 2..].join("/")
    }
}

fn first_line(s: &str, max: usize) -> String {
    let line = s.lines().next().unwrap_or(s);
    if line.len() <= max {
        line.to_string()
    } else {
        format!("{}…", &line[..max - 1])
    }
}
