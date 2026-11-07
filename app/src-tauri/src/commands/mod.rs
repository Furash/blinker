//! Tauri command layer: JSON in/out, shared state, events.

mod config_cmds;
mod get_state;
mod import_cmds;
mod launch;
mod process;
mod startup_cmds;
mod state;
mod theme_cmds;
mod ui;
mod updater_cmds;

// Globs (not named re-exports) so the hidden `__cmd__<name>` items that
// `#[tauri::command]` generates are re-exported too (`generate_handler!` needs them).
pub use config_cmds::*;
pub use get_state::*;
pub use import_cmds::*;
pub use launch::*;
pub use process::*;
pub use startup_cmds::*;
pub use state::*;
pub use theme_cmds::*;
pub use ui::*;
pub use updater_cmds::*;
