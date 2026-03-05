use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
    ExecutableCommand,
};
use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Gauge, Paragraph, Row, Table, TableState},
    Frame, Terminal,
};
use std::{
    env,
    fs,
    io::{self, stdout},
    path::{Path, PathBuf},
    process::Command,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
    thread,
    time::Duration,
};
use walkdir::WalkDir;

// --- Language detection rules ---

struct LangRule {
    name: &'static str,
    markers: &'static [&'static str],
    dep_dirs: &'static [&'static str],
    color: Color,
}

const LANG_RULES: &[LangRule] = &[
    LangRule {
        name: "Rust",
        markers: &["Cargo.toml"],
        dep_dirs: &["target"],
        color: Color::Red,
    },
    LangRule {
        name: "Node.js",
        markers: &["package.json"],
        dep_dirs: &["node_modules"],
        color: Color::Green,
    },
    LangRule {
        name: "Python",
        markers: &["pyproject.toml", "setup.py", "requirements.txt"],
        dep_dirs: &["venv", ".venv", "__pycache__"],
        color: Color::Yellow,
    },
    LangRule {
        name: "Java",
        markers: &["build.gradle", "build.gradle.kts", "pom.xml"],
        dep_dirs: &["build", ".gradle", "target"],
        color: Color::Cyan,
    },
    LangRule {
        name: "Go",
        markers: &["go.mod"],
        dep_dirs: &["vendor"],
        color: Color::Blue,
    },
    LangRule {
        name: "C/C++",
        markers: &["CMakeLists.txt"],
        dep_dirs: &["build"],
        color: Color::Magenta,
    },
    LangRule {
        name: ".NET",
        markers: &["*.csproj", "*.sln"],
        dep_dirs: &["bin", "obj"],
        color: Color::LightCyan,
    },
];

// --- Clean target ---

#[derive(Clone)]
enum CleanTarget {
    Directories(Vec<PathBuf>),
    DockerImage { id: String },
}

// --- Project entry ---

#[derive(Clone)]
struct Project {
    display_name: String,
    lang: &'static str,
    lang_color: Color,
    target: CleanTarget,
    size: u64,
    selected: bool,
}

// --- Scanning ---

fn matches_marker(dir: &Path, marker: &str) -> bool {
    if marker.starts_with('*') {
        let ext = &marker[1..]; // e.g. ".csproj"
        if let Ok(entries) = fs::read_dir(dir) {
            for entry in entries.flatten() {
                if let Some(name) = entry.file_name().to_str() {
                    if name.ends_with(ext) {
                        return true;
                    }
                }
            }
        }
        false
    } else {
        dir.join(marker).exists()
    }
}

fn dir_size(path: &Path) -> u64 {
    WalkDir::new(path)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .filter_map(|e| e.metadata().ok())
        .map(|m| m.len())
        .sum()
}

fn scan_projects(
    root: &Path,
    projects: &Arc<Mutex<Vec<Project>>>,
    scanning_done: &Arc<AtomicBool>,
    dirs_scanned: &Arc<AtomicU64>,
) {
    let walker = WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            let name = e.file_name().to_str().unwrap_or("");
            // Skip hidden dirs and known dep dirs to avoid recursing into them
            !name.starts_with('.')
                && name != "node_modules"
                && name != "target"
                && name != "__pycache__"
                && name != "venv"
                && name != ".venv"
                && name != ".gradle"
                && name != "vendor"
                && name != "bin"
                && name != "obj"
                && name != "build"
        });

    for entry in walker.flatten() {
        if !entry.file_type().is_dir() {
            continue;
        }
        let dir = entry.path();
        dirs_scanned.fetch_add(1, Ordering::Relaxed);

        for rule in LANG_RULES {
            let matched = rule.markers.iter().any(|m| matches_marker(dir, m));
            if !matched {
                continue;
            }

            let mut dep_dirs = Vec::new();
            let mut total_size: u64 = 0;

            for dep_name in rule.dep_dirs {
                // For __pycache__, search recursively
                if *dep_name == "__pycache__" {
                    for sub in WalkDir::new(dir)
                        .into_iter()
                        .filter_map(|e| e.ok())
                        .filter(|e| e.file_type().is_dir() && e.file_name() == "__pycache__")
                    {
                        let p = sub.path().to_path_buf();
                        let s = dir_size(&p);
                        if s > 0 {
                            total_size += s;
                            dep_dirs.push(p);
                        }
                    }
                } else {
                    let dep_path = dir.join(dep_name);
                    if dep_path.is_dir() {
                        let s = dir_size(&dep_path);
                        if s > 0 {
                            total_size += s;
                            dep_dirs.push(dep_path);
                        }
                    }
                }
            }

            if !dep_dirs.is_empty() {
                let short_path = dir
                    .to_str()
                    .unwrap_or("")
                    .replace(&dirs::home_dir_string(), "~");

                let project = Project {
                    display_name: short_path,
                    lang: rule.name,
                    lang_color: rule.color,
                    target: CleanTarget::Directories(dep_dirs),
                    size: total_size,
                    selected: false,
                };
                projects.lock().unwrap().push(project);
            }
            break; // Only match first language rule per directory
        }
    }

    // Scan Docker images after filesystem scan
    scan_docker_images(projects);

    scanning_done.store(true, Ordering::Relaxed);
}

// --- Docker scanning ---

fn parse_docker_size(s: &str) -> u64 {
    let s = s.trim();
    // Docker outputs sizes like "123MB", "1.5GB", "45.3kB"
    let (num_str, unit) = if s.ends_with("GB") {
        (&s[..s.len() - 2], 1024u64 * 1024 * 1024)
    } else if s.ends_with("MB") {
        (&s[..s.len() - 2], 1024u64 * 1024)
    } else if s.ends_with("kB") {
        (&s[..s.len() - 2], 1024u64)
    } else if s.ends_with("B") {
        (&s[..s.len() - 1], 1u64)
    } else {
        return 0;
    };

    num_str
        .trim()
        .parse::<f64>()
        .map(|n| (n * unit as f64) as u64)
        .unwrap_or(0)
}

fn scan_docker_images(projects: &Arc<Mutex<Vec<Project>>>) {
    // Check if docker is available
    let images_output = match Command::new("docker")
        .args(["images", "--format", "{{.ID}}\t{{.Repository}}\t{{.Tag}}\t{{.Size}}"])
        .output()
    {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).to_string(),
        _ => return, // Docker not available or not running
    };

    // Get image IDs that are currently in use by containers
    let used_ids: Vec<String> = Command::new("docker")
        .args(["ps", "-a", "--format", "{{.Image}}"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .map(|s| s.to_string())
                .collect()
        })
        .unwrap_or_default();

    // Also resolve used image names to IDs for accurate matching
    let used_image_ids: Vec<String> = Command::new("docker")
        .args(["ps", "-a", "--format", "{{.Image}}"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| {
            let names: Vec<String> = String::from_utf8_lossy(&o.stdout)
                .lines()
                .map(|s| s.to_string())
                .collect();
            // Resolve each name/tag to an image ID
            let mut ids = Vec::new();
            for name in &names {
                if let Ok(out) = Command::new("docker")
                    .args(["inspect", "--format", "{{.Id}}", name])
                    .output()
                {
                    if out.status.success() {
                        let full_id = String::from_utf8_lossy(&out.stdout).trim().to_string();
                        // Docker short IDs are 12 chars
                        if full_id.starts_with("sha256:") {
                            ids.push(full_id[7..19].to_string());
                        } else if full_id.len() >= 12 {
                            ids.push(full_id[..12].to_string());
                        }
                    }
                }
            }
            ids
        })
        .unwrap_or_default();

    let mut docker_projects = Vec::new();

    for line in images_output.lines() {
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.len() < 4 {
            continue;
        }
        let id = parts[0].trim();
        let repo = parts[1].trim();
        let tag = parts[2].trim();
        let size_str = parts[3].trim();

        // Check if this image is in use (by short ID or name)
        let in_use = used_image_ids.iter().any(|uid| uid == id)
            || used_ids.iter().any(|u| {
                u == id
                    || u == repo
                    || *u == format!("{}:{}", repo, tag)
            });

        if in_use {
            continue;
        }

        let display = if tag == "<none>" && repo == "<none>" {
            format!("<dangling> {}", &id[..12.min(id.len())])
        } else if tag == "<none>" {
            repo.to_string()
        } else {
            format!("{}:{}", repo, tag)
        };

        let size = parse_docker_size(size_str);

        docker_projects.push(Project {
            display_name: display,
            lang: "Docker",
            lang_color: Color::LightBlue,
            target: CleanTarget::DockerImage {
                id: id.to_string(),
            },
            size,
            selected: false,
        });
    }

    if !docker_projects.is_empty() {
        projects.lock().unwrap().extend(docker_projects);
    }
}

// --- Size formatting ---

fn format_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * KB;
    const GB: u64 = 1024 * MB;

    if bytes >= GB {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.0} KB", bytes as f64 / KB as f64)
    } else {
        format!("{} B", bytes)
    }
}

// --- App state ---

enum AppPhase {
    Scanning,
    Selecting,
    Confirming,
    Deleting { current: usize, total: usize },
    Done { freed: u64 },
}

struct App {
    projects: Arc<Mutex<Vec<Project>>>,
    scanning_done: Arc<AtomicBool>,
    dirs_scanned: Arc<AtomicU64>,
    table_state: TableState,
    phase: AppPhase,
}

impl App {
    fn new(root: PathBuf) -> Self {
        let projects = Arc::new(Mutex::new(Vec::new()));
        let scanning_done = Arc::new(AtomicBool::new(false));
        let dirs_scanned = Arc::new(AtomicU64::new(0));

        // Spawn scan thread
        {
            let projects = Arc::clone(&projects);
            let scanning_done = Arc::clone(&scanning_done);
            let dirs_scanned = Arc::clone(&dirs_scanned);
            thread::spawn(move || {
                scan_projects(&root, &projects, &scanning_done, &dirs_scanned);
            });
        }

        App {
            projects,
            scanning_done,
            dirs_scanned,
            table_state: TableState::default().with_selected(0),
            phase: AppPhase::Scanning,
        }
    }

    fn selected_count(&self) -> usize {
        self.projects
            .lock()
            .unwrap()
            .iter()
            .filter(|p| p.selected)
            .count()
    }

    fn selected_size(&self) -> u64 {
        self.projects
            .lock()
            .unwrap()
            .iter()
            .filter(|p| p.selected)
            .map(|p| p.size)
            .sum()
    }

    fn total_size(&self) -> u64 {
        self.projects.lock().unwrap().iter().map(|p| p.size).sum()
    }

    fn project_count(&self) -> usize {
        self.projects.lock().unwrap().len()
    }

    fn toggle_selected(&mut self) {
        if let Some(idx) = self.table_state.selected() {
            let mut projects = self.projects.lock().unwrap();
            if let Some(p) = projects.get_mut(idx) {
                p.selected = !p.selected;
            }
        }
    }

    fn select_all(&mut self) {
        let mut projects = self.projects.lock().unwrap();
        let all_selected = projects.iter().all(|p| p.selected);
        for p in projects.iter_mut() {
            p.selected = !all_selected;
        }
    }

    fn move_up(&mut self) {
        let count = self.project_count();
        if count == 0 {
            return;
        }
        let i = self.table_state.selected().unwrap_or(0);
        let new = if i == 0 { count - 1 } else { i - 1 };
        self.table_state.select(Some(new));
    }

    fn move_down(&mut self) {
        let count = self.project_count();
        if count == 0 {
            return;
        }
        let i = self.table_state.selected().unwrap_or(0);
        let new = if i >= count - 1 { 0 } else { i + 1 };
        self.table_state.select(Some(new));
    }

    fn delete_selected(&mut self) {
        let projects = self.projects.lock().unwrap();
        let to_delete: Vec<(usize, CleanTarget)> = projects
            .iter()
            .enumerate()
            .filter(|(_, p)| p.selected)
            .map(|(i, p)| (i, p.target.clone()))
            .collect();

        let total = to_delete.iter().map(|(_, t)| match t {
            CleanTarget::Directories(dirs) => dirs.len(),
            CleanTarget::DockerImage { .. } => 1,
        }).sum();
        drop(projects);

        self.phase = AppPhase::Deleting { current: 0, total };

        let mut freed: u64 = 0;
        let mut done = 0;

        for (_proj_idx, target) in &to_delete {
            match target {
                CleanTarget::Directories(dirs) => {
                    for dir in dirs {
                        let size = dir_size(dir);
                        let _ = fs::remove_dir_all(dir);
                        freed += size;
                        done += 1;
                        self.phase = AppPhase::Deleting {
                            current: done,
                            total,
                        };
                    }
                }
                CleanTarget::DockerImage { id, .. } => {
                    // Get size before removal (already stored in project)
                    let size = self.projects.lock().unwrap()
                        .iter()
                        .find(|p| matches!(&p.target, CleanTarget::DockerImage { id: pid, .. } if pid == id))
                        .map(|p| p.size)
                        .unwrap_or(0);
                    let _ = Command::new("docker").args(["rmi", id]).output();
                    freed += size;
                    done += 1;
                    self.phase = AppPhase::Deleting {
                        current: done,
                        total,
                    };
                }
            }
        }

        // Remove cleaned entries
        let mut projects = self.projects.lock().unwrap();
        projects.retain(|p| {
            if !p.selected {
                return true;
            }
            match &p.target {
                CleanTarget::Directories(dirs) => dirs.iter().any(|d| d.exists()),
                CleanTarget::DockerImage { id, .. } => {
                    // Check if image still exists
                    Command::new("docker")
                        .args(["inspect", "--type=image", id])
                        .output()
                        .map(|o| o.status.success())
                        .unwrap_or(false)
                }
            }
        });
        for p in projects.iter_mut() {
            p.selected = false;
        }

        self.phase = AppPhase::Done { freed };
    }
}

// --- UI rendering ---

fn ui(frame: &mut Frame, app: &mut App) {
    let area = frame.area();

    match &app.phase {
        AppPhase::Scanning => render_scanning(frame, area, app),
        AppPhase::Selecting => render_selecting(frame, area, app),
        AppPhase::Confirming => render_confirming(frame, area, app),
        AppPhase::Deleting { current, total } => {
            render_deleting(frame, area, *current, *total);
        }
        AppPhase::Done { freed } => render_done(frame, area, *freed),
    }
}

fn render_scanning(frame: &mut Frame, area: Rect, app: &App) {
    let chunks = Layout::vertical([
        Constraint::Fill(1),
        Constraint::Length(3),
        Constraint::Length(3),
        Constraint::Fill(1),
    ])
    .split(area);

    let scanned = app.dirs_scanned.load(Ordering::Relaxed);
    let found = app.project_count();

    let info = Paragraph::new(Line::from(vec![
        Span::raw("Scanning... "),
        Span::styled(
            format!("{} dirs scanned", scanned),
            Style::default().fg(Color::Cyan),
        ),
        Span::raw(" | "),
        Span::styled(
            format!("{} projects found", found),
            Style::default().fg(Color::Green),
        ),
    ]))
    .centered()
    .block(Block::default().borders(Borders::ALL).title(" depclean "));

    // Animated spinner via a simple rotating bar
    let spinner_chars = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
    let idx = (scanned as usize) % spinner_chars.len();
    let spinner = Paragraph::new(Line::from(vec![
        Span::styled(
            format!(" {} ", spinner_chars[idx]),
            Style::default().fg(Color::Yellow),
        ),
        Span::raw("Scanning filesystem..."),
    ]))
    .centered();

    frame.render_widget(spinner, chunks[1]);
    frame.render_widget(info, chunks[2]);
}

fn render_selecting(frame: &mut Frame, area: Rect, app: &mut App) {
    let chunks = Layout::vertical([
        Constraint::Length(3),
        Constraint::Fill(1),
        Constraint::Length(3),
    ])
    .split(area);

    // Header
    let total = app.total_size();
    let selected_size = app.selected_size();
    let header = Paragraph::new(Line::from(vec![
        Span::styled(" depclean ", Style::default().fg(Color::Cyan).bold()),
        Span::raw(format!(
            "— {} projects | Total: {} | Selected: {}",
            app.project_count(),
            format_size(total),
            format_size(selected_size),
        )),
    ]))
    .block(Block::default().borders(Borders::ALL));
    frame.render_widget(header, chunks[0]);

    // Table
    let projects = app.projects.lock().unwrap();
    let rows: Vec<Row> = projects
        .iter()
        .map(|p| {
            let check = if p.selected { "✔" } else { " " };
            let checkbox_style = if p.selected {
                Style::default().fg(Color::Green)
            } else {
                Style::default().fg(Color::DarkGray)
            };

            let target_info = match &p.target {
                CleanTarget::Directories(dirs) => format!("{} dirs", dirs.len()),
                CleanTarget::DockerImage { .. } => "image".to_string(),
            };

            Row::new(vec![
                Cell::from(format!(" [{}]", check)).style(checkbox_style),
                Cell::from(p.display_name.clone()),
                Cell::from(p.lang).style(Style::default().fg(p.lang_color)),
                Cell::from(format_size(p.size)).style(Style::default().fg(Color::Yellow)),
                Cell::from(target_info)
                    .style(Style::default().fg(Color::DarkGray)),
            ])
        })
        .collect();

    let table = Table::new(
        rows,
        [
            Constraint::Length(6),
            Constraint::Fill(1),
            Constraint::Length(10),
            Constraint::Length(12),
            Constraint::Length(8),
        ],
    )
    .header(
        Row::new(vec!["", "Path", "Language", "Size", "Deps"])
            .style(Style::default().bold().fg(Color::White))
            .bottom_margin(1),
    )
    .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED))
    .block(Block::default().borders(Borders::ALL));

    drop(projects);
    frame.render_stateful_widget(table, chunks[1], &mut app.table_state);

    // Footer
    let footer = Paragraph::new(Line::from(vec![
        Span::styled(" ↑↓ ", Style::default().fg(Color::Cyan).bold()),
        Span::raw("navigate  "),
        Span::styled("Space ", Style::default().fg(Color::Cyan).bold()),
        Span::raw("toggle  "),
        Span::styled("a ", Style::default().fg(Color::Cyan).bold()),
        Span::raw("select all  "),
        Span::styled("Enter ", Style::default().fg(Color::Green).bold()),
        Span::raw("delete  "),
        Span::styled("q ", Style::default().fg(Color::Red).bold()),
        Span::raw("quit"),
    ]))
    .block(Block::default().borders(Borders::ALL));
    frame.render_widget(footer, chunks[2]);
}

fn render_confirming(frame: &mut Frame, area: Rect, app: &App) {
    let chunks = Layout::vertical([
        Constraint::Fill(1),
        Constraint::Length(7),
        Constraint::Fill(1),
    ])
    .split(area);

    let count = app.selected_count();
    let size = app.selected_size();

    let confirm = Paragraph::new(vec![
        Line::from(""),
        Line::from(vec![Span::styled(
            "  ⚠  Confirm Deletion",
            Style::default().fg(Color::Yellow).bold(),
        )]),
        Line::from(""),
        Line::from(format!(
            "  Delete dependencies from {} project(s), freeing ~{}?",
            count,
            format_size(size)
        )),
        Line::from(""),
        Line::from(vec![
            Span::styled("  y ", Style::default().fg(Color::Green).bold()),
            Span::raw("confirm  "),
            Span::styled("n ", Style::default().fg(Color::Red).bold()),
            Span::raw("cancel"),
        ]),
    ])
    .block(Block::default().borders(Borders::ALL).title(" Confirm "));

    frame.render_widget(confirm, chunks[1]);
}

fn render_deleting(frame: &mut Frame, area: Rect, current: usize, total: usize) {
    let chunks = Layout::vertical([
        Constraint::Fill(1),
        Constraint::Length(5),
        Constraint::Fill(1),
    ])
    .split(area);

    let ratio = if total > 0 {
        current as f64 / total as f64
    } else {
        0.0
    };

    let gauge = Gauge::default()
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Deleting... "),
        )
        .gauge_style(Style::default().fg(Color::Red))
        .ratio(ratio)
        .label(format!("{}/{} items", current, total));

    frame.render_widget(gauge, chunks[1]);
}

fn render_done(frame: &mut Frame, area: Rect, freed: u64) {
    let chunks = Layout::vertical([
        Constraint::Fill(1),
        Constraint::Length(5),
        Constraint::Fill(1),
    ])
    .split(area);

    let msg = Paragraph::new(vec![
        Line::from(""),
        Line::from(vec![
            Span::styled("  ✓ ", Style::default().fg(Color::Green).bold()),
            Span::raw(format!("Done! Freed {}", format_size(freed))),
        ]),
        Line::from(vec![
            Span::raw("  Press "),
            Span::styled("q", Style::default().fg(Color::Cyan).bold()),
            Span::raw(" to quit or "),
            Span::styled("Enter", Style::default().fg(Color::Cyan).bold()),
            Span::raw(" to continue"),
        ]),
    ])
    .block(Block::default().borders(Borders::ALL).title(" Complete "));

    frame.render_widget(msg, chunks[1]);
}

// --- Home dir helper ---

mod dirs {
    pub fn home_dir_string() -> String {
        std::env::var("HOME").unwrap_or_default()
    }
}

// --- Tests ---

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    // --- parse_docker_size ---

    #[test]
    fn parse_docker_size_gb() {
        assert_eq!(parse_docker_size("1.5GB"), (1.5 * 1024.0 * 1024.0 * 1024.0) as u64);
    }

    #[test]
    fn parse_docker_size_mb() {
        assert_eq!(parse_docker_size("123MB"), (123.0 * 1024.0 * 1024.0) as u64);
    }

    #[test]
    fn parse_docker_size_kb() {
        assert_eq!(parse_docker_size("45.3kB"), (45.3 * 1024.0) as u64);
    }

    #[test]
    fn parse_docker_size_bytes() {
        assert_eq!(parse_docker_size("512B"), 512);
    }

    #[test]
    fn parse_docker_size_zero_gb() {
        assert_eq!(parse_docker_size("0GB"), 0);
    }

    #[test]
    fn parse_docker_size_with_whitespace() {
        assert_eq!(parse_docker_size("  256MB  "), (256.0 * 1024.0 * 1024.0) as u64);
    }

    #[test]
    fn parse_docker_size_invalid() {
        assert_eq!(parse_docker_size("invalid"), 0);
        assert_eq!(parse_docker_size(""), 0);
    }

    #[test]
    fn parse_docker_size_fractional_gb() {
        assert_eq!(parse_docker_size("0.5GB"), (0.5 * 1024.0 * 1024.0 * 1024.0) as u64);
    }

    // --- format_size ---

    #[test]
    fn format_size_bytes() {
        assert_eq!(format_size(500), "500 B");
    }

    #[test]
    fn format_size_kb() {
        assert_eq!(format_size(2048), "2 KB");
    }

    #[test]
    fn format_size_mb() {
        assert_eq!(format_size(5 * 1024 * 1024), "5.0 MB");
    }

    #[test]
    fn format_size_gb() {
        assert_eq!(format_size(2 * 1024 * 1024 * 1024), "2.0 GB");
    }

    #[test]
    fn format_size_zero() {
        assert_eq!(format_size(0), "0 B");
    }

    // --- matches_marker ---

    #[test]
    fn matches_marker_exact_file() {
        let dir = tempdir();
        fs::write(dir.join("Cargo.toml"), "").unwrap();
        assert!(matches_marker(&dir, "Cargo.toml"));
        assert!(!matches_marker(&dir, "package.json"));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn matches_marker_wildcard() {
        let dir = tempdir();
        fs::write(dir.join("myapp.csproj"), "").unwrap();
        assert!(matches_marker(&dir, "*.csproj"));
        assert!(!matches_marker(&dir, "*.sln"));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn matches_marker_empty_dir() {
        let dir = tempdir();
        assert!(!matches_marker(&dir, "Cargo.toml"));
        assert!(!matches_marker(&dir, "*.csproj"));
        fs::remove_dir_all(&dir).unwrap();
    }

    // --- dir_size ---

    #[test]
    fn dir_size_counts_files() {
        let dir = tempdir();
        fs::write(dir.join("a.txt"), "hello").unwrap(); // 5 bytes
        fs::write(dir.join("b.txt"), "world!").unwrap(); // 6 bytes
        assert_eq!(dir_size(&dir), 11);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn dir_size_empty() {
        let dir = tempdir();
        assert_eq!(dir_size(&dir), 0);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn dir_size_nested() {
        let dir = tempdir();
        let sub = dir.join("sub");
        fs::create_dir(&sub).unwrap();
        fs::write(sub.join("file.txt"), "abc").unwrap(); // 3 bytes
        fs::write(dir.join("root.txt"), "de").unwrap(); // 2 bytes
        assert_eq!(dir_size(&dir), 5);
        fs::remove_dir_all(&dir).unwrap();
    }

    // --- Docker display name logic ---

    #[test]
    fn docker_display_dangling_image() {
        let repo = "<none>";
        let tag = "<none>";
        let id = "abc123def456";
        let display = if tag == "<none>" && repo == "<none>" {
            format!("<dangling> {}", &id[..12.min(id.len())])
        } else if tag == "<none>" {
            repo.to_string()
        } else {
            format!("{}:{}", repo, tag)
        };
        assert_eq!(display, "<dangling> abc123def456");
    }

    #[test]
    fn docker_display_no_tag() {
        let repo = "myimage";
        let tag = "<none>";
        let id = "abc123def456";
        let display = if tag == "<none>" && repo == "<none>" {
            format!("<dangling> {}", &id[..12.min(id.len())])
        } else if tag == "<none>" {
            repo.to_string()
        } else {
            format!("{}:{}", repo, tag)
        };
        assert_eq!(display, "myimage");
    }

    #[test]
    fn docker_display_with_tag() {
        let repo = "nginx";
        let tag = "latest";
        let id = "abc123def456";
        let display = if tag == "<none>" && repo == "<none>" {
            format!("<dangling> {}", &id[..12.min(id.len())])
        } else if tag == "<none>" {
            repo.to_string()
        } else {
            format!("{}:{}", repo, tag)
        };
        assert_eq!(display, "nginx:latest");
    }

    // --- Helper ---

    fn tempdir() -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "depclean_test_{}_{}", std::process::id(), id
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }
}

// --- Main ---

fn main() -> io::Result<()> {
    let root = env::args()
        .nth(1)
        .map(|s| {
            if s == "~" {
                PathBuf::from(dirs::home_dir_string())
            } else if s.starts_with("~/") {
                PathBuf::from(dirs::home_dir_string()).join(&s[2..])
            } else {
                PathBuf::from(s)
            }
        })
        .unwrap_or_else(|| env::current_dir().unwrap());

    if !root.is_dir() {
        eprintln!("Error: {} is not a directory", root.display());
        std::process::exit(1);
    }

    // Setup terminal
    enable_raw_mode()?;
    stdout().execute(EnterAlternateScreen)?;
    let backend = ratatui::backend::CrosstermBackend::new(stdout());
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new(root);

    loop {
        // Transition from Scanning to Selecting when done
        if matches!(app.phase, AppPhase::Scanning) && app.scanning_done.load(Ordering::Relaxed) {
            // Sort by size descending
            let mut projects = app.projects.lock().unwrap();
            projects.sort_by(|a, b| b.size.cmp(&a.size));
            drop(projects);
            app.phase = AppPhase::Selecting;
        }

        terminal.draw(|f| ui(f, &mut app))?;

        // Poll events with timeout for animation during scanning
        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                match &app.phase {
                    AppPhase::Scanning => {
                        if key.code == KeyCode::Char('q') {
                            break;
                        }
                    }
                    AppPhase::Selecting => match key.code {
                        KeyCode::Char('q') | KeyCode::Esc => break,
                        KeyCode::Up | KeyCode::Char('k') => app.move_up(),
                        KeyCode::Down | KeyCode::Char('j') => app.move_down(),
                        KeyCode::Char(' ') => app.toggle_selected(),
                        KeyCode::Char('a') => app.select_all(),
                        KeyCode::Enter => {
                            if app.selected_count() > 0 {
                                app.phase = AppPhase::Confirming;
                            }
                        }
                        _ => {}
                    },
                    AppPhase::Confirming => match key.code {
                        KeyCode::Char('y') => app.delete_selected(),
                        KeyCode::Char('n') | KeyCode::Esc => app.phase = AppPhase::Selecting,
                        _ => {}
                    },
                    AppPhase::Deleting { .. } => {}
                    AppPhase::Done { .. } => match key.code {
                        KeyCode::Char('q') | KeyCode::Esc => break,
                        KeyCode::Enter => app.phase = AppPhase::Selecting,
                        _ => {}
                    },
                }
            }
        }
    }

    // Restore terminal
    disable_raw_mode()?;
    stdout().execute(LeaveAlternateScreen)?;
    Ok(())
}
