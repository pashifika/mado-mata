use super::*;
use crate::host::test_support::{make_host, observe, submit};
use std::thread;

#[test]
fn shared_budget_reserves_multi_owner_operations_atomically() {
    let budget = Arc::new(HandleBudget::new(3));
    let barrier = std::sync::Barrier::new(3);
    let reservations = thread::scope(|scope| {
        let first = scope.spawn(|| {
            barrier.wait();
            budget.reserve(2)
        });
        let second = scope.spawn(|| {
            barrier.wait();
            budget.reserve(2)
        });
        barrier.wait();
        [
            first.join().expect("first allocator"),
            second.join().expect("second allocator"),
        ]
    });
    assert_eq!(
        reservations.iter().filter(|result| result.is_ok()).count(),
        1
    );
    assert_eq!(budget.live.load(Ordering::Acquire), 2);
    assert_eq!(
        budget
            .reserve(2)
            .expect_err("failed group reserves nothing")
            .category,
        "HandleLimit"
    );
    let final_slot = budget.reserve(1).expect("one slot remains");
    assert_eq!(budget.live.load(Ordering::Acquire), 3);
    drop(reservations);
    drop(final_slot);
    assert_eq!(budget.live.load(Ordering::Acquire), 0);
}

#[test]
fn readiness_and_wait_bounds_cannot_be_bypassed() {
    let host = make_host("success", "template-first");
    assert_eq!(
        host.begin_workflow().expect_err("no readiness").category,
        "ReadinessContract"
    );
    assert_eq!(
        host.call("observe", json!({}))
            .expect_err("top-level capture")
            .category,
        "AdmissionClosed"
    );
    host.begin_readiness().expect("readiness");
    let observation = observe(&host);
    assert_eq!(
        submit(&host, &observation, "A")
            .expect_err("readiness input")
            .category,
        "AdmissionClosed"
    );
    for args in [
        json!({}),
        json!({"duration_ms":0}),
        json!({"duration_ms":501}),
        json!({"duration_ms":"1"}),
    ] {
        assert_eq!(
            host.call("wait", args)
                .expect_err("invalid finite wait")
                .category,
            "Argument"
        );
    }
    host.begin_workflow().expect("explicit Ready");
    assert_eq!(
        host.begin_workflow()
            .expect_err("workflow reentry")
            .category,
        "ReadinessContract"
    );
    assert_eq!(host.finish()["clean"], true);
    assert_eq!(
        host.call("observe", json!({}))
            .expect_err("normal return closes admission")
            .category,
        "AdmissionClosed"
    );
}
