use super::*;

#[test]
fn preflight_unverified_native_cleanup_is_independent_of_cache_release() {
    let primary = Fault::new("NativeSession", "session opening failed")
        .with_context(json!({"native_cleanup":"unverified"}));
    let released = preflight_cleanup(&primary, Ok(()));
    assert_eq!(released["clean"], false);
    assert_eq!(released["status"], "IncompleteCleanup");
    assert_eq!(released["native_cleanup"], "unverified");
    let retained = preflight_cleanup(
        &primary,
        Err(Fault::new("RunnerResources", "retained owner")),
    );
    assert_eq!(retained["clean"], false);
    assert_eq!(retained["runner_release"]["category"], "RunnerResources");
    assert_eq!(retained["native_cleanup"], "unverified");
}

#[test]
fn preflight_cleanup_never_parses_human_diagnostics() {
    let primary = Fault::new("Blocked", "native_cleanup unverified; rollback incomplete");
    assert_eq!(preflight_cleanup(&primary, Ok(()))["clean"], true);
    assert_eq!(
        preflight_cleanup(
            &primary,
            Err(Fault::new("RunnerResources", "still retained"))
        )["clean"],
        false
    );
}
