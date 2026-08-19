//! Data source abstraction for sidebar agent monitoring.
//!
//! This module provides a trait-based abstraction for different data sources,
//! allowing the sidebar daemon to read agent state from either tmux (default)
//! or external systems like Squad.

pub mod squad;

use anyhow::Result;

use crate::multiplexer::types::{AgentPane, AgentStatus};

/// A single row in the hierarchical sidebar list (session → worktree → pane).
///
/// This is a pure data-layer type: it carries no UI types and no rendering
/// logic. The Squad data source builds it from `state/window-states` +
/// `state/worktrees` + `state/<id>.meta`; the sidebar daemon ships it inside
/// the snapshot; the sidebar client renders it as an expandable tree.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum SidebarEntry {
    /// Top-level session group (e.g. the tmux session name).
    Session {
        /// Session name (from the `window=` field, before the colon).
        session: String,
        /// Human-readable session label (from `state/worktrees` column 5).
        label: Option<String>,
        /// Number of tasks in this session.
        task_count: usize,
        /// Combined status of every task in the session.
        aggregate_status: Option<AgentStatus>,
    },
    /// Worktree group within a session.
    Worktree {
        /// Session name this worktree belongs to.
        session: String,
        /// Absolute path to the worktree.
        worktree_path: String,
        /// Git branch (from `state/worktrees` column 6), when determinable.
        branch: Option<String>,
        /// Number of tasks in this worktree.
        task_count: usize,
        /// Combined status of every task in the worktree.
        aggregate_status: Option<AgentStatus>,
    },
    /// Individual task pane (leaf).
    Pane {
        /// Squad task ID.
        task_id: String,
        /// Session name the task belongs to.
        session: String,
        /// Window name (the task's window within the session).
        window_name: String,
        /// Synthetic pane ID (`%{task-id}`), used for click-to-focus.
        pane_id: String,
        /// State label from `state/window-states` (working, done, blocked, ...).
        label: String,
        /// Detail prose from `state/window-states`.
        detail: String,
        /// Mapped agent status.
        status: Option<AgentStatus>,
        /// Cached agent identity (e.g. "claude"), from `state/<id>.meta`.
        agent_kind: Option<String>,
        /// Composite command line (e.g. "claude-3-opus (high)"), from meta.
        agent_command: Option<String>,
    },
}

impl SidebarEntry {
    /// Session name this entry belongs to.
    pub fn session(&self) -> &str {
        match self {
            SidebarEntry::Session { session, .. }
            | SidebarEntry::Worktree { session, .. }
            | SidebarEntry::Pane { session, .. } => session,
        }
    }
}

/// Composite key identifying a worktree group within a session.
///
/// Used by the sidebar client's expand/collapse state. The separator is a NUL
/// so the `(session, worktree_path)` pair round-trips unambiguously (paths may
/// contain colons but not NUL bytes).
pub fn worktree_key(session: &str, worktree_path: &str) -> String {
    format!("{session}\u{0}{worktree_path}")
}

/// Trait for providing agent pane data to the sidebar daemon.
///
/// Implementations read agent state from different sources (tmux, Squad, etc.)
/// and return a list of `AgentPane` structs for display in the sidebar.
pub trait DataSource: Send + Sync {
    /// List all currently known agent panes.
    ///
    /// Returns a vector of `AgentPane` structs representing the current state
    /// of all agents tracked by this data source.
    fn list_agents(&self) -> Result<Vec<AgentPane>>;

    /// Get the name of this data source for logging/display.
    fn name(&self) -> &'static str;
}

/// Data source type for CLI selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DataSourceType {
    /// tmux-based monitoring (default, original Workmux behavior)
    #[default]
    Tmux,
    /// Squad state file monitoring
    Squad,
}

impl std::fmt::Display for DataSourceType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DataSourceType::Tmux => write!(f, "tmux"),
            DataSourceType::Squad => write!(f, "squad"),
        }
    }
}

impl std::str::FromStr for DataSourceType {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "tmux" => Ok(DataSourceType::Tmux),
            "squad" => Ok(DataSourceType::Squad),
            other => Err(format!("unknown data source: {}", other)),
        }
    }
}
