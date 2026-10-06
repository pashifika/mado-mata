use crate::model::Fault;
use serde::{Deserialize, Serialize};
use serde_json::Value;

mod child;
mod clock;
mod evidence;
mod payload;
mod protocol;
mod recognition;
mod startup;
mod supervision;
pub(crate) use startup::{NativePreparation, StartupLink, StartupReply, emit_native_transition};

pub use child::child;
pub use evidence::Observer;
#[cfg(feature = "engine")]
pub(crate) use evidence::emit_backend_initialization_started;
pub(crate) use evidence::{emit_host_wait_entered, emit_script_log, emit_vm_hook_reached};
pub(crate) use evidence::{emit_native_preparation, emit_native_status};
pub(crate) use payload::reserve_native_images;
pub use protocol::read_json;
pub use recognition::recognition_child;
pub(crate) use recognition::{run_capabilities, run_trial};
pub(crate) use supervision::run_once_after_milestone;
pub(crate) use supervision::run_prepared_with_executable;

pub(crate) struct PreparedExecution<'a> {
    pub control: &'a crate::model::Control,
    pub modules: Option<&'a crate::typescript::PreparedModules>,
    pub images: Option<&'a crate::images::PayloadReservation>,
    pub startup: Option<&'a NativePreparation>,
}
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
