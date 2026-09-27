use crate::model::Fault;
use serde::{Deserialize, Serialize};
use serde_json::Value;

mod child;
mod evidence;
mod payload;
mod protocol;
mod recognition;
mod supervision;

pub use child::child;
pub use evidence::Observer;
#[cfg(feature = "engine")]
pub(crate) use evidence::emit_backend_initialization_started;
pub(crate) use evidence::{emit_host_wait_entered, emit_script_log, emit_vm_hook_reached};
pub use protocol::read_json;
pub use recognition::recognition_child;
pub(crate) use recognition::{run_capabilities, run_trial};
pub(crate) use supervision::run_once_after_milestone;
pub use supervision::{
    intentional_exit_evidence, parent_loss_evidence, parent_probe,
    run_environment_check_with_executable, run_once, run_once_with_executable, sample,
};

#[derive(Debug, Serialize, Deserialize)]
pub struct RunRecord {
    pub version: u32,
    pub run: String,
    pub candidate: String,
    pub lane: String,
    pub scenario: String,
    pub profile: String,
    pub plan_identity: String,
    pub inventory_identity: String,
    pub status: String,
    pub reason: String,
    pub primary: Option<Fault>,
    pub entry_outcome: String,
    pub cleanup: Value,
    pub observations: Value,
    pub metrics: Value,
    pub milestones: Vec<Value>,
    pub exit_code: Option<i32>,
    pub forced: bool,
    /// The owned child's build identity, or null when startup evidence is unavailable.
    pub build: Value,
}

#[cfg(test)]
mod tests;
