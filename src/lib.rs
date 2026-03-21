pub mod process;
pub mod session;
pub mod sway;

use anyhow::Result;
use std::collections::HashMap;
use std::fs;
use std::time::SystemTime;

pub struct SessionInfo {
    pub name: String,
    pub cwd: String,
    pub pid: u32,
    pub status: SessionStatus,
    pub doing: String,
    pub session_id: Option<String>,
    pub branch: Option<String>,
    pub task_num: Option<u8>,
    pub task_name: Option<String>,
    pub workspace: Option<String>,
    pub activity_log: Option<Vec<session::ActivityEntry>>,
}

pub enum SessionStatus {
    Working,
    Idle(chrono::Duration),
    Stuck,
    Stopped,
}

impl SessionStatus {
    pub fn css_class(&self) -> &'static str {
        match self {
            SessionStatus::Working => "status-working",
            SessionStatus::Idle(_) => "status-idle",
            SessionStatus::Stuck => "status-stuck",
            SessionStatus::Stopped => "status-stopped",
        }
    }

    pub fn dot(&self) -> &'static str {
        match self {
            SessionStatus::Working => "●",
            SessionStatus::Idle(_) => "○",
            SessionStatus::Stuck => "⚠",
            SessionStatus::Stopped => "◌",
        }
    }

    pub fn label(&self) -> String {
        match self {
            SessionStatus::Working => "working".to_string(),
            SessionStatus::Idle(dur) => {
                let mins = dur.num_minutes();
                if mins < 1 {
                    "idle".to_string()
                } else if mins < 60 {
                    format!("idle {mins}m")
                } else if mins < 1440 {
                    format!("idle {}h", mins / 60)
                } else {
                    format!("idle {}d", mins / 1440)
                }
            }
            SessionStatus::Stuck => "stuck".to_string(),
            SessionStatus::Stopped => "stopped".to_string(),
        }
    }
}

/// Read baton hook state file for a given cwd.
/// Written by ~/.claude/hooks/baton-status.sh
/// Format: line 1 = "working"|"idle", line 2 = tool name, line 3 = session_id
fn read_hook_status(cwd: &str) -> Option<(String, Option<String>)> {
    let state_dir = dirs::home_dir()?.join(".local/state/baton");
    let key = cwd.replace('/', "-");
    let path = state_dir.join(&key);

    let content = fs::read_to_string(&path).ok()?;
    let mut lines = content.lines();
    let status = lines.next()?.to_string();
    let tool = lines.next().map(|s| s.to_string()).filter(|s| !s.is_empty());

    // Also check the file's mtime — if the status file is stale (>60s),
    // don't trust it (session may have crashed without writing "idle")
    let mtime = fs::metadata(&path).ok()?.modified().ok()?;
    let age = SystemTime::now().duration_since(mtime).ok()?;
    if age.as_secs() > 120 {
        return None; // stale, ignore
    }

    Some((status, tool))
}

pub fn gather_sessions() -> Result<Vec<SessionInfo>> {
    let claude_procs = process::find_claude_processes()?;
    let claude_home = dirs::home_dir()
        .ok_or_else(|| anyhow::anyhow!("no home dir"))?
        .join(".claude")
        .join("projects");

    let pid_ws_map = sway::build_pid_workspace_map();

    let mut sessions = Vec::new();
    let mut seen_cwds: HashMap<String, bool> = HashMap::new();

    for proc in &claude_procs {
        if seen_cwds.contains_key(&proc.cwd) {
            continue;
        }
        seen_cwds.insert(proc.cwd.clone(), true);

        // Primary signal: hook state file
        let hook_status = read_hook_status(&proc.cwd);

        // Parse JSONL for "doing" text, branch, stuck detection
        let session_data = session::find_latest_session(&claude_home, &proc.cwd);

        let (doing, session_id, branch, activity_log, repeated_edits, has_subagents) =
            match &session_data {
                Some(data) => (
                    data.last_doing.clone().unwrap_or_default(),
                    data.session_id.clone(),
                    data.branch.clone(),
                    Some(data.activity_log.clone()),
                    data.repeated_edits,
                    data.has_active_subagents,
                ),
                None => (String::new(), None, None, None, 0, false),
            };

        // Determine status from hook state (primary) or fallback to file mtime
        // Subagents override: if subagents are active, session is working regardless
        let status = if repeated_edits >= 3 {
            SessionStatus::Stuck
        } else if has_subagents {
            SessionStatus::Working
        } else if let Some((ref hook_state, _)) = hook_status {
            match hook_state.as_str() {
                "working" => SessionStatus::Working,
                "idle" => {
                    let state_dir = dirs::home_dir()
                        .unwrap()
                        .join(".local/state/baton");
                    let key = proc.cwd.replace('/', "-");
                    let idle_secs = fs::metadata(state_dir.join(&key))
                        .ok()
                        .and_then(|m| m.modified().ok())
                        .and_then(|mt| SystemTime::now().duration_since(mt).ok())
                        .map(|d| d.as_secs())
                        .unwrap_or(0);
                    SessionStatus::Idle(chrono::Duration::seconds(idle_secs as i64))
                }
                _ => SessionStatus::Working,
            }
        } else {
            // No hook state — fallback to JSONL mtime
            let project_dir_name = proc.cwd.replace('/', "-");
            let project_dir = claude_home.join(&project_dir_name);
            let idle_secs = latest_jsonl_mtime(&project_dir)
                .and_then(|mt| SystemTime::now().duration_since(mt).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);
            if idle_secs < 15 {
                SessionStatus::Working
            } else {
                SessionStatus::Idle(chrono::Duration::seconds(idle_secs as i64))
            }
        };

        let task_info = sway::find_task_for_pid(proc.pid, &pid_ws_map);
        let name = derive_name(&proc.cwd, branch.as_deref());

        sessions.push(SessionInfo {
            name,
            cwd: proc.cwd.clone(),
            pid: proc.pid,
            status,
            doing,
            session_id,
            branch,
            task_num: task_info.as_ref().map(|t| t.task_num),
            task_name: task_info.as_ref().map(|t| t.task_name.clone()),
            workspace: task_info.as_ref().map(|t| t.workspace.clone()),
            activity_log,
        });
    }

    sessions.sort_by(|a, b| {
        a.task_num
            .unwrap_or(99)
            .cmp(&b.task_num.unwrap_or(99))
            .then(a.name.cmp(&b.name))
    });
    Ok(sessions)
}

fn latest_jsonl_mtime(project_dir: &std::path::Path) -> Option<SystemTime> {
    std::fs::read_dir(project_dir)
        .ok()?
        .filter_map(|e| e.ok())
        .filter(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            name.ends_with(".jsonl") && !name.starts_with("agent-")
        })
        .filter_map(|e| e.metadata().ok()?.modified().ok())
        .max()
}

fn derive_name(cwd: &str, branch: Option<&str>) -> String {
    if let Some(b) = branch {
        if b != "main" && b != "master" && b != "develop" {
            return b.to_string();
        }
    }
    std::path::Path::new(cwd)
        .file_name()
        .map(|f| f.to_string_lossy().to_string())
        .unwrap_or_else(|| cwd.to_string())
}

pub fn shorten_path(path: &str, max_len: usize) -> String {
    let home = dirs::home_dir().map(|h| h.to_string_lossy().to_string());
    let shortened = match &home {
        Some(h) if path.starts_with(h.as_str()) => format!("~{}", &path[h.len()..]),
        _ => path.to_string(),
    };
    let char_count = shortened.chars().count();
    if char_count <= max_len {
        shortened
    } else {
        let skip = char_count - max_len + 1;
        let tail: String = shortened.chars().skip(skip).collect();
        format!("…{tail}")
    }
}

pub fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{truncated}…")
    }
}
