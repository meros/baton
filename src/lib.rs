pub mod process;
pub mod session;
pub mod sway;

use anyhow::Result;
use std::collections::HashMap;

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
                let label = if mins < 1 {
                    String::new()
                } else if mins < 60 {
                    format!("{mins}m")
                } else if mins < 1440 {
                    format!("{}h", mins / 60)
                } else {
                    format!("{}d", mins / 1440)
                };
                if label.is_empty() {
                    "idle".to_string()
                } else {
                    format!("idle {label}")
                }
            }
            SessionStatus::Stuck => "stuck".to_string(),
            SessionStatus::Stopped => "stopped".to_string(),
        }
    }
}

pub fn gather_sessions() -> Result<Vec<SessionInfo>> {
    let claude_procs = process::find_claude_processes()?;
    let claude_home = dirs::home_dir()
        .ok_or_else(|| anyhow::anyhow!("no home dir"))?
        .join(".claude")
        .join("projects");

    // Build sway PID→workspace map once
    let pid_ws_map = sway::build_pid_workspace_map();

    let mut sessions = Vec::new();
    let mut seen_cwds: HashMap<String, bool> = HashMap::new();

    for proc in &claude_procs {
        if seen_cwds.contains_key(&proc.cwd) {
            continue;
        }
        seen_cwds.insert(proc.cwd.clone(), true);

        let session_data = session::find_latest_session(&claude_home, &proc.cwd);

        let (status, doing, session_id, branch, activity_log) = match session_data {
            Some(data) => {
                let status = infer_status(&data, true);
                let doing = data.last_doing.unwrap_or_default();
                (
                    status,
                    doing,
                    data.session_id,
                    data.branch,
                    Some(data.activity_log),
                )
            }
            None => (SessionStatus::Working, String::new(), None, None, None),
        };

        // Find task info for this process
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

    // Sort by task number first, then name
    sessions.sort_by(|a, b| {
        a.task_num
            .unwrap_or(99)
            .cmp(&b.task_num.unwrap_or(99))
            .then(a.name.cmp(&b.name))
    });
    Ok(sessions)
}

fn infer_status(data: &session::SessionData, process_alive: bool) -> SessionStatus {
    if !process_alive {
        return SessionStatus::Stopped;
    }

    let now = chrono::Utc::now();
    let idle_duration = now - data.last_activity;

    if data.repeated_edits >= 3 {
        return SessionStatus::Stuck;
    }

    if idle_duration > chrono::Duration::minutes(2) {
        return SessionStatus::Idle(idle_duration);
    }

    SessionStatus::Working
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
    if shortened.len() <= max_len {
        shortened
    } else {
        format!("…{}", &shortened[shortened.len() - max_len + 1..])
    }
}

pub fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[..max - 1])
    }
}
