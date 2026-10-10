//! Main-window-only dispatch seam for the local collaboration transport.

use super::{Backend, background};
use mado_mata_desktop::collaboration::{Claimed, Ready};
use mado_runtime_comparison::model::Fault;
use serde_json::Value;

pub const MAIN_WINDOW: &str = "main";
pub const REQUEST_EVENT: &str = "collaboration-request";

fn require_main_window(label: &str) -> Result<(), Fault> {
    if label == MAIN_WINDOW {
        Ok(())
    } else {
        Err(Fault::new(
            "CollaborationWindow",
            "Only the main window serves local collaboration",
        ))
    }
}

/// Starts or attaches the endpoint once configuration is ready. A refusal
/// leaves ordinary authoring unaffected.
#[tauri::command]
pub async fn collaboration_ready(
    window: tauri::WebviewWindow,
    state: tauri::State<'_, Backend>,
) -> Result<Ready, Fault> {
    require_main_window(window.label())?;
    state.bootstrap.application()?;
    let collaboration = state.collaboration.clone();
    background(move || collaboration.ready()).await
}

#[tauri::command]
pub fn collaboration_unavailable(
    window: tauri::WebviewWindow,
    state: tauri::State<'_, Backend>,
) -> Result<(), Fault> {
    require_main_window(window.label())?;
    state.collaboration.unavailable();
    Ok(())
}

#[tauri::command]
pub fn collaboration_claim(
    ticket: String,
    window: tauri::WebviewWindow,
    state: tauri::State<'_, Backend>,
) -> Result<Option<Claimed>, Fault> {
    require_main_window(window.label())?;
    Ok(state.collaboration.claim(&ticket))
}

#[tauri::command]
pub fn collaboration_reply(
    ticket: String,
    response: Value,
    window: tauri::WebviewWindow,
    state: tauri::State<'_, Backend>,
) -> Result<(), Fault> {
    require_main_window(window.label())?;
    state.collaboration.reply(&ticket, response)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_main_window_serves_collaboration() {
        require_main_window(MAIN_WINDOW).unwrap();
        for label in ["recognition-preview", "", "main-other"] {
            assert_eq!(
                require_main_window(label).unwrap_err().category,
                "CollaborationWindow"
            );
        }
    }
}
