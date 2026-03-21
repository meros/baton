use anyhow::Result;
use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::process::Command;

/// Task info from sway
pub struct TaskInfo {
    pub task_num: u8,
    pub task_name: String,
    pub workspace: String,
}

/// Read task names from ~/.config/sway/task-names.json
pub fn read_task_names() -> HashMap<u8, String> {
    let path = dirs::config_dir()
        .map(|d| d.join("sway/task-names.json"))
        .unwrap_or_default();

    let content = match fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return HashMap::new(),
    };

    let parsed: HashMap<String, String> = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(_) => return HashMap::new(),
    };

    parsed
        .into_iter()
        .filter_map(|(k, v)| k.parse::<u8>().ok().map(|n| (n, v)))
        .collect()
}

/// Get the sway tree and build a PID → workspace mapping
pub fn build_pid_workspace_map() -> HashMap<u32, String> {
    let mut map = HashMap::new();

    let output = match Command::new("swaymsg").args(["-t", "get_tree"]).output() {
        Ok(o) => o,
        Err(_) => return map,
    };

    let tree: Value = match serde_json::from_slice(&output.stdout) {
        Ok(v) => v,
        Err(_) => return map,
    };

    collect_pids(&tree, None, &mut map);
    map
}

/// Recursively walk the sway tree, tracking the current workspace name
fn collect_pids(node: &Value, workspace: Option<&str>, map: &mut HashMap<u32, String>) {
    let node_type = node.get("type").and_then(|t| t.as_str()).unwrap_or("");

    // Track workspace name as we descend
    let ws = if node_type == "workspace" {
        node.get("name").and_then(|n| n.as_str())
    } else {
        workspace
    };

    // If this node has a PID, record it
    if let Some(pid) = node.get("pid").and_then(|p| p.as_u64()) {
        if let Some(ws_name) = ws {
            map.insert(pid as u32, ws_name.to_string());
        }
    }

    // Recurse into child nodes
    if let Some(nodes) = node.get("nodes").and_then(|n| n.as_array()) {
        for child in nodes {
            collect_pids(child, ws, map);
        }
    }
    if let Some(nodes) = node.get("floating_nodes").and_then(|n| n.as_array()) {
        for child in nodes {
            collect_pids(child, ws, map);
        }
    }
}

/// Extract task number from workspace name like "5:t2a" → 2
pub fn task_num_from_workspace(ws: &str) -> Option<u8> {
    // Format: "N:tXY" where X is task number
    let after_colon = ws.split(':').nth(1)?;
    let after_t = after_colon.strip_prefix('t')?;
    // First char is the task number
    after_t.chars().next()?.to_digit(10).map(|n| n as u8)
}

/// Find which task a given PID belongs to by walking up the process tree
/// to find a PID that's in the sway window tree
pub fn find_task_for_pid(pid: u32, pid_ws_map: &HashMap<u32, String>) -> Option<TaskInfo> {
    let task_names = read_task_names();
    let mut current_pid = pid;

    // Walk up the process tree (max 20 levels to avoid infinite loops)
    for _ in 0..20 {
        if let Some(ws) = pid_ws_map.get(&current_pid) {
            if let Some(task_num) = task_num_from_workspace(ws) {
                let task_name = task_names
                    .get(&task_num)
                    .cloned()
                    .unwrap_or_else(|| format!("Task {task_num}"));
                return Some(TaskInfo {
                    task_num,
                    task_name,
                    workspace: ws.clone(),
                });
            }
        }

        // Read parent PID from /proc/<pid>/stat
        let stat = match fs::read_to_string(format!("/proc/{current_pid}/stat")) {
            Ok(s) => s,
            Err(_) => break,
        };

        // Format: "pid (comm) state ppid ..."
        // Need to skip past the comm field which may contain spaces/parens
        let ppid = parse_ppid_from_stat(&stat);
        match ppid {
            Some(p) if p > 1 => current_pid = p,
            _ => break,
        }
    }

    None
}

fn parse_ppid_from_stat(stat: &str) -> Option<u32> {
    // Find the last ')' to skip past the comm field
    let after_comm = stat.rsplit_once(')')?.1;
    // Fields after comm: state ppid ...
    let mut fields = after_comm.split_whitespace();
    fields.next()?; // state
    fields.next()?.parse().ok() // ppid
}

/// Switch to a workspace via swaymsg
pub fn switch_to_workspace(workspace: &str) -> Result<()> {
    Command::new("swaymsg")
        .args(["workspace", workspace])
        .output()?;
    Ok(())
}

/// Make the baton window floating and sticky
pub fn make_sticky() {
    let _ = Command::new("swaymsg")
        .args(["[app_id=dev.baton.overlay]", "floating", "enable,", "sticky", "enable"])
        .output();
    // Alternative format if the above doesn't work
    let _ = Command::new("sh")
        .args(["-c", r#"swaymsg '[app_id="dev.baton.overlay"] floating enable, sticky enable'"#])
        .output();
}
