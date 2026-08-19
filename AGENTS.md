# Workmux Fork - Squad Integration

This is a fork of [workmux](https://github.com/raine/workmux) with added support for reading agent state from Squad's state directory.

## Key Architecture

### Data Source Abstraction

The fork adds a `DataSource` trait abstraction in `src/data_source/mod.rs` that allows the sidebar daemon to read agent state from different sources:

- **tmux** (default): Original Workmux behavior, polls tmux for pane state
- **squad**: Reads from Squad's `state/` directory structure

### Squad Data Source

`src/data_source/squad.rs` implements `SquadDataSource` which reads:

1. `state/window-states` (TSV: window, id, label, state, detail) — task ground truth
2. `state/<id>.meta` (shell `key=value`, one per line: model, effort, kind, mode, worktree, project)
3. `state/<id>.busy-gen` (mtime for elapsed time)

### Sidebar Hierarchy (session → worktree → pane)

The Squad sidebar renders an expandable hierarchy, not just a flat list:

- `state/worktrees` (TSV: `session\tworktree_path\tproject\ttask_ids\tsession_label\tworktree_branch`) is the hierarchy overlay published by Squad. When absent, groups are derived from window-states + meta.
- `SquadDataSource::build_hierarchy()` (pure data layer, no UI) produces `Vec<SidebarEntry>` (Session/Worktree/Pane variants with aggregate status). `list_entries()` is the daemon-facing entry point.
- The daemon ships `entries` + `hierarchy_enabled` in each snapshot; Squad mode enables the hierarchy by default.
- `SidebarApp` keeps `entries`, `expanded_session_keys` / `expanded_worktree_keys` (everything collapsed by default), and `visible_rows` (`TreeRow`s driving render/navigation/hit-test). The most-recently-active task is always revealed beneath collapsed groups (Herdr pattern).
- Navigation: `Enter` toggles a group, `Right`/`l` expands-and-descends, `Left`/`h` collapses-or-ascends.

### Key Files

- `src/data_source/mod.rs` - DataSource trait/DataSourceType + shared `SidebarEntry` type
- `src/data_source/squad.rs` - SquadDataSource, `read_worktrees()`, `build_hierarchy()`, `list_entries()`
- `src/command/sidebar/daemon.rs` - Daemon with `run_with_data_source()` / `run_squad_daemon()`
- `src/command/sidebar/app.rs` - SidebarApp tree state + navigation
- `src/command/sidebar/ui.rs` - `render_tree_list()` tree-drawing render
- `src/command/sidebar/runtime.rs` - sidebar key handling
- `src/cli.rs` - CLI flag `--data-source` (+ `--instance-id`) on `_sidebar-daemon` command

### Usage

```bash
# Start sidebar daemon reading from Squad
workmux _sidebar-daemon --data-source squad

# Default tmux behavior (backward compatible)
workmux _sidebar-daemon --data-source tmux
```

### Environment Variables

- `SQUAD_BASE` - Primary Squad base directory (takes precedence)
- `SQUAD_HOME` - Legacy Squad home directory (fallback)
- Default: `~/.fob/squad/`

## Maintaining this file

Keep this file for knowledge useful to almost every future agent session in this project.
Do not repeat what the codebase already shows; point to the authoritative file or command instead.
Prefer rewriting or pruning existing entries over appending new ones.
When updating this file, preserve this bar for all agents and keep entries concise.
