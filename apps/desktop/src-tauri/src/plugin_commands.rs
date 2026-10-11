//! Main-window-only commands for the application-managed OMP adapter.
//!
//! They require normal setup admission but hold no workspace or Edit lock while
//! the client runs. The collaboration bridge exposes no plugin management.

use super::collaboration_commands::MAIN_WINDOW;
use super::{Backend, background};
use mado_mata_desktop::plugin_management::{PluginAction, PluginView};
use mado_runtime_comparison::model::Fault;

fn require_main_window(label: &str) -> Result<(), Fault> {
    if label == MAIN_WINDOW {
        Ok(())
    } else {
        Err(Fault::new(
            "PluginWindow",
            "Only the main window manages plugins",
        ))
    }
}

/// Inspects the discovered OMP, or the explicitly selected executable.
#[tauri::command]
pub async fn plugin_inspect(
    executable: Option<String>,
    window: tauri::WebviewWindow,
    state: tauri::State<'_, Backend>,
) -> Result<PluginView, Fault> {
    require_main_window(window.label())?;
    state.bootstrap.application()?;
    let plugins = state.plugins.clone();
    background(move || plugins.inspect(executable.as_deref())).await
}

/// Applies an action to the host-owned target and source of a reviewed observation.
#[tauri::command]
pub async fn plugin_apply(
    observation: String,
    action: PluginAction,
    window: tauri::WebviewWindow,
    state: tauri::State<'_, Backend>,
) -> Result<PluginView, Fault> {
    require_main_window(window.label())?;
    state.bootstrap.application()?;
    let plugins = state.plugins.clone();
    background(move || plugins.apply(&observation, action)).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_main_window_manages_plugins() {
        require_main_window(MAIN_WINDOW).unwrap();
        for label in ["recognition-preview", "", "main-other"] {
            assert_eq!(
                require_main_window(label).unwrap_err().category,
                "PluginWindow"
            );
        }
    }
}
