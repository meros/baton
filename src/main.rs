use anyhow::Result;
use baton::{gather_sessions, shorten_path, truncate, SessionStatus};
use clap::{Parser, Subcommand};
use colored::Colorize;

#[derive(Parser)]
#[command(name = "baton", about = "Situational awareness for Claude Code sessions")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Watch sessions with auto-refresh
    Watch {
        /// Refresh interval in seconds
        #[arg(short, long, default_value = "2")]
        interval: u64,
    },
    /// Show recent activity log for a session
    Log {
        /// Session name or cwd substring
        session: String,
        /// Number of recent entries
        #[arg(short = 'n', long, default_value = "20")]
        lines: usize,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        None => show_status()?,
        Some(Command::Watch { interval }) => watch_loop(interval)?,
        Some(Command::Log { session, lines }) => show_log(&session, lines)?,
    }

    Ok(())
}

fn show_status() -> Result<()> {
    let sessions = gather_sessions()?;

    if sessions.is_empty() {
        println!("{}", "No active Claude Code sessions found.".dimmed());
        return Ok(());
    }

    println!(
        "  {:<20} {:<42} {:<10} {}",
        "NAME".dimmed(),
        "PWD".dimmed(),
        "STATUS".dimmed(),
        "DOING".dimmed(),
    );

    for s in &sessions {
        let (status_icon, status_text) = format_status(&s.status);
        let name = truncate(&s.name, 20);
        let cwd = shorten_path(&s.cwd, 42);
        let doing = truncate(&s.doing, 50);

        println!(
            "  {:<20} {:<42} {} {:<8} {}",
            name, cwd, status_icon, status_text, doing
        );
    }

    println!();
    Ok(())
}

fn watch_loop(interval: u64) -> Result<()> {
    loop {
        print!("\x1B[2J\x1B[H");
        println!("{}", "baton watch".bold().dimmed());
        println!();
        show_status()?;
        std::thread::sleep(std::time::Duration::from_secs(interval));
    }
}

fn show_log(session_filter: &str, lines: usize) -> Result<()> {
    let sessions = gather_sessions()?;
    let s = sessions
        .iter()
        .find(|s| {
            s.name.contains(session_filter)
                || s.cwd.contains(session_filter)
                || s.session_id
                    .as_deref()
                    .is_some_and(|id| id.starts_with(session_filter))
        })
        .ok_or_else(|| anyhow::anyhow!("No session matching '{session_filter}'"))?;

    println!("{} {}", "Session:".dimmed(), s.name.bold());
    println!("{} {}", "PWD:".dimmed(), s.cwd);
    if let Some(branch) = &s.branch {
        println!("{} {}", "Branch:".dimmed(), branch);
    }
    println!();

    if let Some(ref log) = s.activity_log {
        let start = log.len().saturating_sub(lines);
        for entry in &log[start..] {
            let time = entry.time.format("%H:%M:%S").to_string().dimmed();
            let kind = match entry.kind.as_str() {
                "tool_use" => entry.kind.yellow(),
                "assistant" => entry.kind.cyan(),
                "user" => entry.kind.green(),
                "error" => entry.kind.red(),
                _ => entry.kind.normal(),
            };
            println!(
                "  {} {:<12} {}",
                time,
                kind,
                truncate(&entry.summary, 80)
            );
        }
    } else {
        println!("{}", "  No activity log available.".dimmed());
    }

    Ok(())
}

fn format_status(status: &SessionStatus) -> (colored::ColoredString, colored::ColoredString) {
    match status {
        SessionStatus::Working => ("●".green(), "working".green()),
        SessionStatus::Idle(dur) => {
            let mins = dur.num_minutes();
            let label = if mins < 1 {
                "idle".to_string()
            } else {
                format!("idle {mins}m")
            };
            ("○".yellow(), label.yellow())
        }
        SessionStatus::Stuck => ("⚠".red(), "stuck".red()),
        SessionStatus::Stopped => ("◌".dimmed(), "stopped".dimmed()),
    }
}
