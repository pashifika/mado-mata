use super::{ControllerView, DesktopController, StartRequest, manual_plan, workflow};
use crate::inventory::Inventory;
use crate::model::identity;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

pub(super) fn fixture() -> Inventory {
    Inventory::capture(
        Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/typescript")),
        &manual_plan().unwrap().limits,
    )
    .unwrap()
}

pub(super) fn request(inventory: &Inventory) -> StartRequest {
    StartRequest {
        package_path: concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/typescript").into(),
        inventory_identity: inventory.identity.clone(),
        package_id: inventory.package_id.clone(),
        schema_identity: identity(&inventory.schema).unwrap(),
        profile_id: "saved-choice".into(),
        values: inventory.profiles["ocr-first"]["options"].clone(),
        lane: "controlled".into(),
        scenario: workflow(),
        replay_descriptor_path: None,
        native_intent: None,
        function: None,
    }
}

pub(super) fn settled(controller: &DesktopController) -> ControllerView {
    settled_with_timeout(controller, Duration::from_secs(5))
}

pub(super) fn settled_with_timeout(
    controller: &DesktopController,
    timeout: Duration,
) -> ControllerView {
    let deadline = Instant::now() + timeout;
    loop {
        let view = controller.poll();
        if view.state == "terminal" {
            return view;
        }
        assert!(
            Instant::now() < deadline,
            "operation did not settle: {view:?}"
        );
        thread::sleep(Duration::from_millis(2));
    }
}
