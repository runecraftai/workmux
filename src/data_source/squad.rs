//! Squad state file data source for sidebar monitoring.
//!
//! Reads agent state from Squad's `state/` directory structure:
//! - `state/window-states` (TSV: window, id, label, state, detail)
//! - `state/<id>.meta` (JSON: model, effort, kind, mode)
//! - `state/<id>.busy-gen` (mtime for elapsed time)

use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use crate::multiplexer::types::{AgentPane, AgentStatus};

use super::{DataSource, SidebarEntry};

/// Squad task metadata from `state/<id>.meta` files.
#[derive(Debug, Deserialize, Default)]
pub struct SquadMeta {
    /// LLM model name (e.g., "claude-3-opus", "gpt-4")
    #[serde(default)]
    pub model: Option<String>,
    /// Effort level (e.g., "low", "medium", "high", "xhigh")
    #[serde(default)]
    pub effort: Option<String>,
    /// Task kind (e.g., "strike", "recon", "xo")
    #[serde(default)]
    pub kind: Option<String>,
    /// Task mode (e.g., "drill", "direct-pr", "local-only")
    #[serde(default)]
    pub mode: Option<String>,
    /// Project name or path
    #[serde(default)]
    pub project: Option<String>,
    /// Working directory / worktree path
    #[serde(default)]
    pub worktree: Option<String>,
}

/// A single row from Squad's `state/window-states` TSV file.
#[derive(Debug, Clone)]
pub struct WindowStateEntry {
    /// Window target (e.g., "squad:0" or just window name)
    pub window: String,
    /// Task ID
    pub id: String,
    /// State label (working, done, blocked, etc.)
    pub label: String,
    /// State verb (working, idle, etc.)
    pub state: String,
    /// Detail text
    pub detail: String,
}

/// One row from Squad's `state/worktrees` TSV file: a session × worktree group.
///
/// Format: `session\tworktree_path\tproject\ttask_ids\tsession_label\tworktree_branch`
/// where `task_ids` is a comma-separated list of member task IDs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeGroup {
    /// Session name (from the `window=` field, before the colon).
    pub session: String,
    /// Absolute path to the worktree.
    pub worktree_path: String,
    /// Project path from meta.
    pub project: Option<String>,
    /// Task IDs in this worktree (informational; task truth lives in window-states).
    pub task_ids: Vec<String>,
    /// Human-readable session label.
    pub session_label: Option<String>,
    /// Git branch, when determinable.
    pub worktree_branch: Option<String>,
}

/// Squad data source that reads from Squad's state directory.
pub struct SquadDataSource {
    /// Path to Squad's state directory (e.g., `/path/to/squad/state/`)
    state_dir: PathBuf,
}

impl SquadDataSource {
    /// Create a new SquadDataSource with the given state directory path.
    pub fn new(state_dir: PathBuf) -> Self {
        Self { state_dir }
    }

    /// Get the state directory path.
    pub fn state_dir(&self) -> &Path {
        &self.state_dir
    }

    /// Create a SquadDataSource using the SQUAD_BASE or SQUAD_HOME environment variable.
    ///
    /// Falls back to `~/.fob/squad/state/` if neither is set.
    pub fn from_env() -> Result<Self> {
        let base = if let Ok(base) = std::env::var("SQUAD_BASE") {
            PathBuf::from(base)
        } else if let Ok(home) = std::env::var("SQUAD_HOME") {
            PathBuf::from(home)
        } else {
            let home = home::home_dir()
                .ok_or_else(|| anyhow::anyhow!("Could not determine home directory"))?;
            home.join(".fob").join("squad")
        };

        let state_dir = base.join("state");
        Ok(Self { state_dir })
    }

    /// Parse the `state/window-states` TSV file.
    ///
    /// Format: window\tid\tlabel\tstate\tdetail
    fn read_window_states(&self) -> Result<Vec<WindowStateEntry>> {
        let path = self.state_dir.join("window-states");
        if !path.exists() {
            return Ok(Vec::new());
        }

        let content = fs::read_to_string(&path)
            .with_context(|| format!("Failed to read window-states: {}", path.display()))?;

        let mut entries = Vec::new();
        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }

            let parts: Vec<&str> = line.split('\t').collect();
            if parts.len() >= 5 {
                entries.push(WindowStateEntry {
                    window: parts[0].to_string(),
                    id: parts[1].to_string(),
                    label: parts[2].to_string(),
                    state: parts[3].to_string(),
                    detail: parts[4..].join("\t"), // Join remaining parts as detail
                });
            } else if parts.len() >= 4 {
                entries.push(WindowStateEntry {
                    window: parts[0].to_string(),
                    id: parts[1].to_string(),
                    label: parts[2].to_string(),
                    state: parts[3].to_string(),
                    detail: String::new(),
                });
            }
        }

        Ok(entries)
    }

    /// Parse the `state/worktrees` TSV file (the hierarchy overlay).
    ///
    /// Format: `session\tworktree_path\tproject\ttask_ids\tsession_label\tworktree_branch`
    /// Columns 3-6 are optional; rows with fewer than 2 columns are skipped.
    /// Returns an empty vec when the file is absent (the flat sidebar keeps
    /// working, and `build_hierarchy` derives groups from window-states).
    pub fn read_worktrees(&self) -> Result<Vec<WorktreeGroup>> {
        let path = self.state_dir.join("worktrees");
        if !path.exists() {
            return Ok(Vec::new());
        }

        let content = fs::read_to_string(&path)
            .with_context(|| format!("Failed to read worktrees: {}", path.display()))?;

        let mut groups = Vec::new();
        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let parts: Vec<&str> = line.split('\t').collect();
            if parts.len() < 2 {
                continue;
            }
            let task_ids: Vec<String> = parts
                .get(3)
                .map(|s| {
                    s.split(',')
                        .map(str::trim)
                        .filter(|t| !t.is_empty())
                        .map(String::from)
                        .collect()
                })
                .unwrap_or_default();
            groups.push(WorktreeGroup {
                session: parts[0].to_string(),
                worktree_path: parts[1].to_string(),
                project: parts
                    .get(2)
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_string()),
                task_ids,
                session_label: parts
                    .get(4)
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_string()),
                worktree_branch: parts
                    .get(5)
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_string()),
            });
        }

        Ok(groups)
    }

    /// Read the full hierarchical entry list (session → worktree → pane).
    ///
    /// Pure data layer: no UI dependency. Combines `state/window-states`
    /// (task ground truth), `state/worktrees` (grouping overlay) and
    /// `state/<id>.meta` (model/effort/kind) into a depth-first ordered tree.
    pub fn list_entries(&self) -> Result<Vec<SidebarEntry>> {
        let entries = self.read_window_states()?;
        let worktrees = self.read_worktrees()?;
        let mut metas: HashMap<String, SquadMeta> = HashMap::new();
        for entry in &entries {
            if let Ok(meta) = self.read_meta(&entry.id) {
                metas.insert(entry.id.clone(), meta);
            }
        }
        Ok(Self::build_hierarchy(&worktrees, &entries, &metas))
    }

    /// Build the hierarchical entry list from parsed state files.
    ///
    /// Pure function (no filesystem, no UI): deterministic and unit-testable.
    ///
    /// - Sessions are ordered by name; worktrees within a session by path;
    ///   panes within a worktree by task id.
    /// - When `worktrees` is empty (file not published yet), groups are
    ///   derived from the window-states entries + metas so the hierarchy
    ///   still works during the publisher rollout.
    /// - A task entry whose `(session, meta-path)` matches no published group
    ///   is still emitted via a derived group (publisher-lag tolerance).
    pub fn build_hierarchy(
        worktrees: &[WorktreeGroup],
        entries: &[WindowStateEntry],
        metas: &HashMap<String, SquadMeta>,
    ) -> Vec<SidebarEntry> {
        // Merge published groups with derived groups so every window-states
        // entry lands somewhere in the tree.
        let mut groups = worktrees.to_vec();
        let derived = derive_worktree_groups(entries, metas);
        let published_keys: HashSet<(String, String)> = groups
            .iter()
            .map(|g| (g.session.clone(), g.worktree_path.clone()))
            .collect();
        for group in derived {
            if !published_keys.contains(&(group.session.clone(), group.worktree_path.clone())) {
                groups.push(group);
            }
        }

        // Session → worktree membership, ordered.
        let mut session_names: Vec<String> = Vec::new();
        let mut groups_by_session: HashMap<String, Vec<&WorktreeGroup>> = HashMap::new();
        for group in &groups {
            if !groups_by_session.contains_key(&group.session) {
                session_names.push(group.session.clone());
            }
            groups_by_session
                .entry(group.session.clone())
                .or_default()
                .push(group);
        }
        session_names.sort();
        for session_groups in groups_by_session.values_mut() {
            session_groups.sort_by(|a, b| a.worktree_path.cmp(&b.worktree_path));
        }

        // Panes grouped by (session, worktree path from meta), entry order.
        let mut panes_by_key: HashMap<(String, String), Vec<&WindowStateEntry>> = HashMap::new();
        for entry in entries {
            let (session, _) = Self::parse_window_target(&entry.window);
            let path = entry_worktree_path(entry, metas);
            panes_by_key.entry((session, path)).or_default().push(entry);
        }
        for panes in panes_by_key.values_mut() {
            panes.sort_by(|a, b| a.id.cmp(&b.id));
        }

        let mut out: Vec<SidebarEntry> = Vec::new();
        for session_name in &session_names {
            let Some(session_groups) = groups_by_session.get(session_name) else {
                continue;
            };
            let session_label = session_groups.iter().find_map(|g| g.session_label.clone());

            let mut session_panes: Vec<&WindowStateEntry> = Vec::new();
            let mut session_status = None;
            for group in session_groups {
                let key = (session_name.clone(), group.worktree_path.clone());
                let panes = panes_by_key.get(&key).map(Vec::as_slice).unwrap_or(&[]);
                session_panes.extend(panes.iter().copied());
                for pane in panes {
                    session_status = combine_status(session_status, Self::map_status(&pane.label));
                }
            }

            out.push(SidebarEntry::Session {
                session: session_name.clone(),
                label: session_label,
                task_count: session_panes.len(),
                aggregate_status: session_status,
            });

            for group in session_groups {
                let key = (session_name.clone(), group.worktree_path.clone());
                let panes = panes_by_key.get(&key).map(Vec::as_slice).unwrap_or(&[]);
                let mut wt_status = None;
                for pane in panes {
                    wt_status = combine_status(wt_status, Self::map_status(&pane.label));
                }

                out.push(SidebarEntry::Worktree {
                    session: session_name.clone(),
                    worktree_path: group.worktree_path.clone(),
                    branch: group.worktree_branch.clone(),
                    task_count: panes.len(),
                    aggregate_status: wt_status,
                });

                for entry in panes {
                    out.push(Self::pane_entry(entry, metas));
                }
            }
        }
        out
    }

    /// Build a `SidebarEntry::Pane` from a window-states row + meta.
    fn pane_entry(entry: &WindowStateEntry, metas: &HashMap<String, SquadMeta>) -> SidebarEntry {
        let (session, window_name) = Self::parse_window_target(&entry.window);
        let meta = metas.get(&entry.id);
        let agent_command = match (
            meta.and_then(|m| m.model.as_ref()),
            meta.and_then(|m| m.effort.as_ref()),
        ) {
            (Some(model), Some(effort)) => Some(format!("{model} ({effort})")),
            (Some(model), None) => Some(model.clone()),
            (None, Some(effort)) => Some(format!("({effort})")),
            (None, None) => None,
        };
        SidebarEntry::Pane {
            task_id: entry.id.clone(),
            session,
            window_name,
            pane_id: format!("%{}", entry.id),
            label: entry.label.clone(),
            detail: entry.detail.clone(),
            status: Self::map_status(&entry.label),
            agent_kind: meta.and_then(|m| m.kind.clone()),
            agent_command,
        }
    }

    /// Read task metadata from `state/<id>.meta`.
    ///
    /// Squad writes meta files in shell key=value format (one per line),
    /// not JSON. Example:
    ///   window=Squad:sq-abc
    ///   model=claude-3-opus
    ///   effort=high
    ///   kind=strike
    fn read_meta(&self, task_id: &str) -> Result<SquadMeta> {
        let path = self.state_dir.join(format!("{}.meta", task_id));
        if !path.exists() {
            return Ok(SquadMeta::default());
        }

        let content = fs::read_to_string(&path)
            .with_context(|| format!("Failed to read meta: {}", path.display()))?;

        let mut meta = SquadMeta::default();
        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some((key, value)) = line.split_once('=') {
                let value = value.trim();
                if value.is_empty() || value == "-" {
                    continue;
                }
                match key.trim() {
                    "model" => meta.model = Some(value.to_string()),
                    "effort" => meta.effort = Some(value.to_string()),
                    "kind" => meta.kind = Some(value.to_string()),
                    "mode" => meta.mode = Some(value.to_string()),
                    "project" => meta.project = Some(value.to_string()),
                    "worktree" => meta.worktree = Some(value.to_string()),
                    _ => {} // Ignore unknown fields
                }
            }
        }
        Ok(meta)
    }

    /// Get the mtime of `state/<id>.busy-gen` as a Unix timestamp.
    ///
    /// Returns None if the file doesn't exist.
    fn read_busy_gen_mtime(&self, task_id: &str) -> Option<u64> {
        let path = self.state_dir.join(format!("{}.busy-gen", task_id));
        fs::metadata(&path)
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
    }

    /// Map a Squad state label to a Workmux AgentStatus.
    fn map_status(label: &str) -> Option<AgentStatus> {
        match label.to_lowercase().as_str() {
            "working" => Some(AgentStatus::Working),
            "done" => Some(AgentStatus::Done),
            "blocked" | "awaiting-decision" => Some(AgentStatus::Waiting),
            "failed" => Some(AgentStatus::Done), // Show as done with error styling
            "idle" | _ => None,
        }
    }

    /// Parse the window target into session and window name.
    ///
    /// Input format: "session:window" or just "window"
    /// Returns: (session, window_name)
    fn parse_window_target(window: &str) -> (String, String) {
        if let Some((session, name)) = window.split_once(':') {
            (session.to_string(), name.to_string())
        } else {
            ("squad".to_string(), window.to_string())
        }
    }
}

impl DataSource for SquadDataSource {
    fn list_agents(&self) -> Result<Vec<AgentPane>> {
        let entries = self.read_window_states()?;
        let mut agents = Vec::new();

        for entry in entries {
            let meta = self.read_meta(&entry.id).unwrap_or_default();
            let busy_gen_mtime = self.read_busy_gen_mtime(&entry.id);
            let (session, window_name) = Self::parse_window_target(&entry.window);

            // Build the pane title from state and detail
            let pane_title = if entry.detail.is_empty() {
                Some(entry.state.clone())
            } else {
                Some(format!("{}: {}", entry.state, entry.detail))
            };

            // Build agent command from model and effort
            let agent_command = match (&meta.model, &meta.effort) {
                (Some(model), Some(effort)) => Some(format!("{} ({})", model, effort)),
                (Some(model), None) => Some(model.clone()),
                (None, Some(effort)) => Some(format!("({})", effort)),
                (None, None) => None,
            };

            let agent = AgentPane {
                session,
                window_name,
                pane_id: format!("%{}", entry.id),
                window_id: String::new(),
                window_index: None,
                path: meta
                    .worktree
                    .map(PathBuf::from)
                    .or_else(|| meta.project.map(PathBuf::from))
                    .unwrap_or_else(|| PathBuf::from("/")),
                pane_title,
                status: Self::map_status(&entry.label),
                status_ts: busy_gen_mtime,
                updated_ts: busy_gen_mtime,
                window_cmd: None,
                agent_command,
                agent_kind: meta.kind,
            };

            agents.push(agent);
        }

        Ok(agents)
    }

    fn name(&self) -> &'static str {
        "squad"
    }
}

/// The worktree path for a window-states entry: `meta.worktree` → `meta.project` → `/`.
fn entry_worktree_path(entry: &WindowStateEntry, metas: &HashMap<String, SquadMeta>) -> String {
    metas
        .get(&entry.id)
        .and_then(|m| m.worktree.clone())
        .or_else(|| metas.get(&entry.id).and_then(|m| m.project.clone()))
        .unwrap_or_else(|| "/".to_string())
}

/// Derive worktree groups from window-states + metas.
///
/// Used as a fallback when the `state/worktrees` file has not been published
/// yet, and as a safety net for entries the publisher hasn't grouped.
fn derive_worktree_groups(
    entries: &[WindowStateEntry],
    metas: &HashMap<String, SquadMeta>,
) -> Vec<WorktreeGroup> {
    let mut groups: HashMap<(String, String), WorktreeGroup> = HashMap::new();
    for entry in entries {
        let (session, _) = SquadDataSource::parse_window_target(&entry.window);
        let worktree_path = entry_worktree_path(entry, metas);
        let meta = metas.get(&entry.id);
        let group = groups
            .entry((session.clone(), worktree_path.clone()))
            .or_insert_with(|| WorktreeGroup {
                session: session.clone(),
                worktree_path: worktree_path.clone(),
                project: meta.and_then(|m| m.project.clone()),
                task_ids: Vec::new(),
                session_label: None,
                worktree_branch: None,
            });
        group.task_ids.push(entry.id.clone());
    }
    let mut out: Vec<WorktreeGroup> = groups.into_values().collect();
    out.sort_by(|a, b| {
        a.session
            .cmp(&b.session)
            .then(a.worktree_path.cmp(&b.worktree_path))
    });
    out
}

/// Combine two statuses into the group aggregate: Working > Waiting > Done > None.
fn combine_status(a: Option<AgentStatus>, b: Option<AgentStatus>) -> Option<AgentStatus> {
    match (a, b) {
        (Some(AgentStatus::Working), _) | (_, Some(AgentStatus::Working)) => {
            Some(AgentStatus::Working)
        }
        (Some(AgentStatus::Waiting), _) | (_, Some(AgentStatus::Waiting)) => {
            Some(AgentStatus::Waiting)
        }
        (Some(AgentStatus::Done), _) | (_, Some(AgentStatus::Done)) => Some(AgentStatus::Done),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn create_test_state_dir() -> TempDir {
        let dir = TempDir::new().unwrap();
        let state_dir = dir.path().join("state");
        fs::create_dir_all(&state_dir).unwrap();
        dir
    }

    #[test]
    fn test_parse_window_target() {
        let (session, window) = SquadDataSource::parse_window_target("squad:my-window");
        assert_eq!(session, "squad");
        assert_eq!(window, "my-window");

        let (session, window) = SquadDataSource::parse_window_target("just-window");
        assert_eq!(session, "squad");
        assert_eq!(window, "just-window");
    }

    #[test]
    fn test_map_status() {
        assert_eq!(
            SquadDataSource::map_status("working"),
            Some(AgentStatus::Working)
        );
        assert_eq!(SquadDataSource::map_status("done"), Some(AgentStatus::Done));
        assert_eq!(
            SquadDataSource::map_status("blocked"),
            Some(AgentStatus::Waiting)
        );
        assert_eq!(
            SquadDataSource::map_status("awaiting-decision"),
            Some(AgentStatus::Waiting)
        );
        assert_eq!(
            SquadDataSource::map_status("failed"),
            Some(AgentStatus::Done)
        );
        assert_eq!(SquadDataSource::map_status("idle"), None);
        assert_eq!(SquadDataSource::map_status("unknown"), None);
    }

    #[test]
    fn test_read_window_states_empty() {
        let dir = create_test_state_dir();
        let state_dir = dir.path().join("state");
        let ds = SquadDataSource::new(state_dir);

        let entries = ds.read_window_states().unwrap();
        assert!(entries.is_empty());
    }

    #[test]
    fn test_read_window_states() {
        let dir = create_test_state_dir();
        let state_dir = dir.path().join("state");

        let content = "squad:main\ttask-1\tworking\tworking\tImplementing feature\n\
                        squad:main\ttask-2\tdone\tdone\tFeature complete\n";
        fs::write(state_dir.join("window-states"), content).unwrap();

        let ds = SquadDataSource::new(state_dir);
        let entries = ds.read_window_states().unwrap();

        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].id, "task-1");
        assert_eq!(entries[0].label, "working");
        assert_eq!(entries[0].detail, "Implementing feature");
        assert_eq!(entries[1].id, "task-2");
        assert_eq!(entries[1].label, "done");
    }

    #[test]
    fn test_read_meta() {
        let dir = create_test_state_dir();
        let state_dir = dir.path().join("state");

        // Squad writes meta in shell key=value format, not JSON
        let meta_content = "window=Squad:sq-abc\nmodel=claude-3-opus\neffort=high\nkind=strike\nmode=drill\nproject=my-project\nworktree=/tmp/worktree-123\n";
        fs::write(state_dir.join("task-1.meta"), meta_content).unwrap();

        let ds = SquadDataSource::new(state_dir);
        let meta = ds.read_meta("task-1").unwrap();

        assert_eq!(meta.model.unwrap(), "claude-3-opus");
        assert_eq!(meta.effort.unwrap(), "high");
        assert_eq!(meta.kind.unwrap(), "strike");
        assert_eq!(meta.worktree.unwrap(), "/tmp/worktree-123");
    }

    #[test]
    fn test_read_meta_empty_values() {
        let dir = create_test_state_dir();
        let state_dir = dir.path().join("state");

        // Squad uses '-' for unset values
        let meta_content = "model=-\neffort=-\nkind=strike\n";
        fs::write(state_dir.join("task-1.meta"), meta_content).unwrap();

        let ds = SquadDataSource::new(state_dir);
        let meta = ds.read_meta("task-1").unwrap();

        assert!(meta.model.is_none());
        assert!(meta.effort.is_none());
        assert_eq!(meta.kind.unwrap(), "strike");
    }

    #[test]
    fn test_list_agents_full() {
        let dir = create_test_state_dir();
        let state_dir = dir.path().join("state");

        // Write window-states
        let content = "squad:main\ttask-1\tworking\tworking\tImplementing feature\n";
        fs::write(state_dir.join("window-states"), content).unwrap();

        // Write meta in shell key=value format
        let meta_content =
            "model=claude-3-opus\neffort=high\nkind=strike\nworktree=/tmp/worktree-123\n";
        fs::write(state_dir.join("task-1.meta"), meta_content).unwrap();

        let ds = SquadDataSource::new(state_dir);
        let agents = ds.list_agents().unwrap();

        assert_eq!(agents.len(), 1);
        let agent = &agents[0];
        assert_eq!(agent.session, "squad");
        assert_eq!(agent.window_name, "main");
        assert_eq!(agent.pane_id, "%task-1");
        assert_eq!(agent.status, Some(AgentStatus::Working));
        assert_eq!(agent.path, PathBuf::from("/tmp/worktree-123"));
        assert_eq!(agent.agent_kind.as_ref().unwrap(), "strike");
        assert!(
            agent
                .agent_command
                .as_ref()
                .unwrap()
                .contains("claude-3-opus")
        );
    }

    fn entry(id: &str, window: &str, label: &str, state: &str, detail: &str) -> WindowStateEntry {
        WindowStateEntry {
            window: window.to_string(),
            id: id.to_string(),
            label: label.to_string(),
            state: state.to_string(),
            detail: detail.to_string(),
        }
    }

    fn meta(model: Option<&str>, worktree: Option<&str>, kind: Option<&str>) -> SquadMeta {
        SquadMeta {
            model: model.map(String::from),
            worktree: worktree.map(String::from),
            kind: kind.map(String::from),
            ..Default::default()
        }
    }

    #[test]
    fn test_read_worktrees_parses_realistic_file() {
        let dir = create_test_state_dir();
        let state_dir = dir.path().join("state");
        let content = "Squad\t/home/rehem/.fob/squad-b4752b/1/squad\t/home/rehem/Projects/squad\tsq-abc,sq-def\tSquad base\tmain\n\
                       Squad\t/tmp/squad-strike-xyz\t/home/rehem/Projects/squad\tsq-ghi\tSquad base\tfix/sidebar\n";
        fs::write(state_dir.join("worktrees"), content).unwrap();

        let ds = SquadDataSource::new(state_dir);
        let groups = ds.read_worktrees().unwrap();

        assert_eq!(groups.len(), 2);
        let g0 = &groups[0];
        assert_eq!(g0.session, "Squad");
        assert_eq!(g0.worktree_path, "/home/rehem/.fob/squad-b4752b/1/squad");
        assert_eq!(g0.project.as_deref(), Some("/home/rehem/Projects/squad"));
        assert_eq!(g0.task_ids, vec!["sq-abc", "sq-def"]);
        assert_eq!(g0.session_label.as_deref(), Some("Squad base"));
        assert_eq!(g0.worktree_branch.as_deref(), Some("main"));
        assert_eq!(groups[1].worktree_branch.as_deref(), Some("fix/sidebar"));
    }

    #[test]
    fn test_read_worktrees_missing_file_returns_empty() {
        let dir = create_test_state_dir();
        let ds = SquadDataSource::new(dir.path().join("state"));
        assert!(ds.read_worktrees().unwrap().is_empty());
    }

    #[test]
    fn test_read_worktrees_skips_short_rows() {
        let dir = create_test_state_dir();
        let state_dir = dir.path().join("state");
        fs::write(state_dir.join("worktrees"), "Squad\nSquad\t/path\n").unwrap();
        let ds = SquadDataSource::new(state_dir);
        let groups = ds.read_worktrees().unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].worktree_path, "/path");
    }

    #[test]
    fn test_build_hierarchy_appendix_b() {
        // The recon report's Appendix B scenario: 1 session, 2 worktrees, 3 tasks.
        let metas = HashMap::from([
            (
                "sq-abc".to_string(),
                meta(
                    Some("claude-3-opus"),
                    Some("/home/rehem/.fob/squad-b4752b/1/squad"),
                    Some("strike"),
                ),
            ),
            (
                "sq-def".to_string(),
                meta(None, Some("/home/rehem/.fob/squad-b4752b/1/squad"), None),
            ),
            (
                "sq-ghi".to_string(),
                meta(None, Some("/tmp/squad-strike-xyz"), None),
            ),
        ]);
        let worktrees = vec![
            WorktreeGroup {
                session: "Squad".to_string(),
                worktree_path: "/home/rehem/.fob/squad-b4752b/1/squad".to_string(),
                project: Some("/home/rehem/Projects/squad".to_string()),
                task_ids: vec!["sq-abc".to_string(), "sq-def".to_string()],
                session_label: Some("Squad base".to_string()),
                worktree_branch: Some("main".to_string()),
            },
            WorktreeGroup {
                session: "Squad".to_string(),
                worktree_path: "/tmp/squad-strike-xyz".to_string(),
                project: Some("/home/rehem/Projects/squad".to_string()),
                task_ids: vec!["sq-ghi".to_string()],
                session_label: Some("Squad base".to_string()),
                worktree_branch: Some("fix/sidebar".to_string()),
            },
        ];
        let entries = vec![
            entry(
                "sq-abc",
                "Squad:sq-abc",
                "working",
                "working",
                "Implementing feature",
            ),
            entry("sq-def", "Squad:sq-def", "done", "done", "Feature complete"),
            entry(
                "sq-ghi",
                "Squad:sq-ghi",
                "working",
                "working",
                "Sidebar hierarchy",
            ),
        ];

        let hierarchy = SquadDataSource::build_hierarchy(&worktrees, &entries, &metas);

        assert_eq!(hierarchy.len(), 6); // 1 session + 2 worktrees + 3 panes

        let SidebarEntry::Session {
            session,
            label,
            task_count,
            aggregate_status,
        } = &hierarchy[0]
        else {
            panic!("expected session first");
        };
        assert_eq!(session, "Squad");
        assert_eq!(label.as_deref(), Some("Squad base"));
        assert_eq!(*task_count, 3);
        assert_eq!(*aggregate_status, Some(AgentStatus::Working));

        let SidebarEntry::Worktree {
            worktree_path,
            branch,
            task_count,
            aggregate_status,
            ..
        } = &hierarchy[1]
        else {
            panic!("expected worktree second");
        };
        assert_eq!(worktree_path, "/home/rehem/.fob/squad-b4752b/1/squad");
        assert_eq!(branch.as_deref(), Some("main"));
        assert_eq!(*task_count, 2);
        assert_eq!(*aggregate_status, Some(AgentStatus::Working));

        let SidebarEntry::Pane {
            task_id,
            pane_id,
            session,
            window_name,
            status,
            agent_kind,
            agent_command,
            ..
        } = &hierarchy[2]
        else {
            panic!("expected pane third");
        };
        assert_eq!(task_id, "sq-abc");
        assert_eq!(pane_id, "%sq-abc");
        assert_eq!(session, "Squad");
        assert_eq!(window_name, "sq-abc");
        assert_eq!(*status, Some(AgentStatus::Working));
        assert_eq!(agent_kind.as_deref(), Some("strike"));
        assert!(agent_command.as_deref().unwrap().contains("claude-3-opus"));

        // Panes within a worktree are sorted by task id.
        let SidebarEntry::Pane { task_id, .. } = &hierarchy[3] else {
            panic!("expected second pane");
        };
        assert_eq!(task_id, "sq-def");

        let SidebarEntry::Worktree {
            worktree_path,
            branch,
            task_count,
            ..
        } = &hierarchy[4]
        else {
            panic!("expected second worktree");
        };
        assert_eq!(worktree_path, "/tmp/squad-strike-xyz");
        assert_eq!(branch.as_deref(), Some("fix/sidebar"));
        assert_eq!(*task_count, 1);

        let SidebarEntry::Pane { task_id, .. } = &hierarchy[5] else {
            panic!("expected third pane");
        };
        assert_eq!(task_id, "sq-ghi");
    }

    #[test]
    fn test_build_hierarchy_derives_groups_without_worktrees_file() {
        // Publisher rollout: no state/worktrees yet, groups derived from metas.
        let metas = HashMap::from([(
            "sq-abc".to_string(),
            meta(None, Some("/home/rehem/.fob/squad-b4752b/1/squad"), None),
        )]);
        let entries = vec![entry("sq-abc", "Squad:sq-abc", "working", "working", "x")];

        let hierarchy = SquadDataSource::build_hierarchy(&[], &entries, &metas);

        assert_eq!(hierarchy.len(), 3); // session + worktree + pane
        let SidebarEntry::Session { task_count, .. } = &hierarchy[0] else {
            panic!()
        };
        assert_eq!(*task_count, 1);
        let SidebarEntry::Worktree { worktree_path, .. } = &hierarchy[1] else {
            panic!()
        };
        assert_eq!(worktree_path, "/home/rehem/.fob/squad-b4752b/1/squad");
        let SidebarEntry::Pane { task_id, .. } = &hierarchy[2] else {
            panic!()
        };
        assert_eq!(task_id, "sq-abc");
    }

    #[test]
    fn test_build_hierarchy_emits_unpublished_entries() {
        // Publisher lag: an entry whose (session, path) isn't in the file yet
        // must still appear (via a derived group).
        let metas = HashMap::from([
            (
                "sq-abc".to_string(),
                meta(None, Some("/home/rehem/.fob/squad-b4752b/1/squad"), None),
            ),
            (
                "sq-new".to_string(),
                meta(None, Some("/tmp/squad-strike-new"), None),
            ),
        ]);
        let worktrees = vec![WorktreeGroup {
            session: "Squad".to_string(),
            worktree_path: "/home/rehem/.fob/squad-b4752b/1/squad".to_string(),
            project: None,
            task_ids: vec!["sq-abc".to_string()],
            session_label: None,
            worktree_branch: None,
        }];
        let entries = vec![
            entry("sq-abc", "Squad:sq-abc", "working", "working", "x"),
            entry("sq-new", "Squad:sq-new", "working", "working", "y"),
        ];

        let hierarchy = SquadDataSource::build_hierarchy(&worktrees, &entries, &metas);
        assert_eq!(hierarchy.len(), 5); // session + 2 worktrees + 2 panes
        let task_ids: Vec<&str> = hierarchy
            .iter()
            .filter_map(|e| match e {
                SidebarEntry::Pane { task_id, .. } => Some(task_id.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(task_ids, vec!["sq-abc", "sq-new"]);
    }

    #[test]
    fn test_combine_status_priority() {
        assert_eq!(
            combine_status(Some(AgentStatus::Working), Some(AgentStatus::Done)),
            Some(AgentStatus::Working)
        );
        assert_eq!(
            combine_status(Some(AgentStatus::Done), Some(AgentStatus::Waiting)),
            Some(AgentStatus::Waiting)
        );
        assert_eq!(
            combine_status(Some(AgentStatus::Done), Some(AgentStatus::Done)),
            Some(AgentStatus::Done)
        );
        assert_eq!(
            combine_status(Some(AgentStatus::Done), None),
            Some(AgentStatus::Done)
        );
        assert_eq!(
            combine_status(None, Some(AgentStatus::Waiting)),
            Some(AgentStatus::Waiting)
        );
        assert_eq!(combine_status(None, None), None);
    }

    #[test]
    fn test_list_entries_full_roundtrip() {
        // A realistic state dir exercising the executable public interface:
        // window-states + worktrees + metas on disk, hierarchy out the other side.
        let dir = create_test_state_dir();
        let state_dir = dir.path().join("state");
        fs::write(
            state_dir.join("window-states"),
            "Squad:sq-abc\tsq-abc\tworking\tworking\tImplementing feature\n\
             Squad:sq-def\tsq-def\tdone\tdone\tFeature complete\n",
        )
        .unwrap();
        fs::write(
            state_dir.join("worktrees"),
            "Squad\t/home/rehem/.fob/squad-b4752b/1/squad\t/home/rehem/Projects/squad\tsq-abc,sq-def\tSquad base\tmain\n",
        )
        .unwrap();
        fs::write(
            state_dir.join("sq-abc.meta"),
            "window=Squad:sq-abc\nmodel=claude-3-opus\neffort=high\nkind=strike\nworktree=/home/rehem/.fob/squad-b4752b/1/squad\n",
        )
        .unwrap();
        fs::write(
            state_dir.join("sq-def.meta"),
            "window=Squad:sq-def\nworktree=/home/rehem/.fob/squad-b4752b/1/squad\n",
        )
        .unwrap();

        let ds = SquadDataSource::new(state_dir);
        let hierarchy = ds.list_entries().unwrap();

        assert_eq!(hierarchy.len(), 4); // session + worktree + 2 panes
        let SidebarEntry::Session {
            session,
            task_count,
            ..
        } = &hierarchy[0]
        else {
            panic!()
        };
        assert_eq!(session, "Squad");
        assert_eq!(*task_count, 2);
        let SidebarEntry::Worktree {
            worktree_path,
            branch,
            task_count,
            ..
        } = &hierarchy[1]
        else {
            panic!()
        };
        assert_eq!(worktree_path, "/home/rehem/.fob/squad-b4752b/1/squad");
        assert_eq!(branch.as_deref(), Some("main"));
        assert_eq!(*task_count, 2);
        let pane_ids: Vec<&str> = hierarchy[2..]
            .iter()
            .filter_map(|e| match e {
                SidebarEntry::Pane { pane_id, .. } => Some(pane_id.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(pane_ids, vec!["%sq-abc", "%sq-def"]);
    }
}
