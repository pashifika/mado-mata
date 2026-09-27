use super::test_support::{make_host, observe, ready_host};
use super::*;

fn zone(id: &str, u0: f64, v0: f64, u1: f64, v1: f64) -> Value {
    json!({"id":id,"region":{"u0":u0,"v0":v0,"u1":u1,"v1":v1}})
}

fn request(observation: &Value) -> Value {
    json!({"observation":observation,
        "basis":{"frame_width":observation["width"],"frame_height":observation["height"],
            "content":{"x":0,"y":0,"width":observation["width"],"height":observation["height"]}},
        "zones":[zone("first", 0.0, 0.0, 1.0, 1.0)]})
}

#[test]
fn grouped_scan_controlled_preserves_frame_order_offsets_and_observation_ownership() {
    let mut host = make_host("success", "template-first");
    let inner = Arc::get_mut(&mut host.inner).unwrap();
    inner.plan.limits.handles = 1;
    inner.handle_budget = Arc::new(HandleBudget::new(1));
    host.begin_readiness().unwrap();
    host.begin_workflow().unwrap();
    let visible = lock(&host.inner.state).visible.clone();
    let observation = observe(&host);
    lock(&host.inner.state).visible = "different later frame".into();
    let mut args = request(&observation);
    args["basis"]["content"] = json!({"x":100,"y":80,"width":200,"height":100});
    args["zones"] = json!([
        zone("absent", 0.75, 0.0, 1.0, 1.0),
        // Floor left/top and ceil right/bottom must include the exact fixture bounds.
        zone("present", 0.004, 0.009, 0.198, 0.195)
    ]);
    let before = host.snapshot();
    let result = host.call("scan_ocr_zones", args).unwrap();
    assert_eq!(result["observation"], observation);
    assert!(result.get("id").is_none());
    assert_eq!(
        result["zones"][0],
        json!({"id":"absent","outcome":"no_match","regions":[]})
    );
    assert_eq!(
        result["zones"][1],
        json!({"id":"present","outcome":"recognized","regions":[{
            "text":visible,"confidence":0.98,"bounds":{"x":100.0,"y":80.0,"width":40.0,"height":20.0},
            "geometry":[[100.0,80.0],[140.0,80.0],[140.0,100.0],[100.0,100.0]]
        }]})
    );
    let after = host.snapshot();
    assert_eq!(after["observations"], before["observations"]);
    assert_eq!(
        after["recognitions"].as_u64().unwrap(),
        before["recognitions"].as_u64().unwrap() + 1
    );
    assert_eq!(host.snapshot()["live_handles"], 1);
    host.call("release", json!({"id":observation["id"]}))
        .unwrap();
    assert_eq!(host.snapshot()["live_handles"], 0);
    assert_eq!(host.finish()["clean"], true);
    // Plain snapshots remain usable after the only managed owner is released.
    assert_eq!(result["zones"][1]["regions"][0]["text"], visible);
}

#[test]
fn grouped_scan_rejects_whole_invalid_request_before_controlled_work() {
    let host = ready_host("backend-failure");
    let observation = observe(&host);
    let mut valid = request(&observation);
    valid["zones"]
        .as_array_mut()
        .unwrap()
        .push(zone("later", 0.0, 0.0, 1.0, 1.0));
    for (path, value) in [
        ("/zones", json!([])),
        (
            "/zones",
            Value::Array(
                (0..=crate::ocr_scan::CONTROLLED_MAX_ZONES)
                    .map(|index| zone(&index.to_string(), 0.0, 0.0, 1.0, 1.0))
                    .collect(),
            ),
        ),
        ("/zones/1/id", json!("first")),
        ("/zones/1/id", json!("")),
        ("/zones/1/id", json!("x".repeat(257))),
        ("/zones/1/id", json!("bad\nidentity")),
        ("/zones/1/region/u0", json!(-0.1)),
        ("/zones/1/region/u1", json!(1.1)),
        ("/zones/1/region/u1", json!(0.0)),
        ("/zones/1/region/v1", Value::Null),
        ("/basis/frame_width", json!(641)),
        ("/basis/content/width", json!(0)),
        ("/basis/content/x", json!(1)),
        ("/basis/content/x", json!(0.5)),
    ] {
        let mut invalid = valid.clone();
        *invalid.pointer_mut(path).unwrap() = value;
        assert_eq!(
            host.call("scan_ocr_zones", invalid).unwrap_err().category,
            "Argument",
            "{path}"
        );
    }
    for path in [
        "",
        "/basis",
        "/basis/content",
        "/zones/1",
        "/zones/1/region",
    ] {
        let mut invalid = valid.clone();
        invalid
            .pointer_mut(path)
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert("expected".into(), json!("not a filter"));
        assert_eq!(
            host.call("scan_ocr_zones", invalid).unwrap_err().category,
            "Argument",
            "{path}"
        );
    }
    assert_eq!(host.snapshot()["recognitions"], 0);
    assert_eq!(host.snapshot()["live_handles"], 1);
    assert_eq!(
        host.call("scan_ocr_zones", valid).unwrap_err().category,
        "Backend"
    );
    assert_eq!(host.snapshot()["recognitions"], 1);
    host.call("release", json!({"id":observation["id"]}))
        .unwrap();
    assert_eq!(host.finish()["clean"], true);
}

#[test]
fn grouped_scan_rejects_foreign_modified_released_and_stale_observations() {
    let host = ready_host("success");
    let foreign = ready_host("success");
    let observation = observe(&host);
    let foreign_observation = observe(&foreign);
    assert_eq!(
        host.call("scan_ocr_zones", request(&foreign_observation))
            .unwrap_err()
            .category,
        "StaleIdentity"
    );
    let mut modified = observation.clone();
    modified["frame"] = json!(999);
    assert_eq!(
        host.call("scan_ocr_zones", request(&modified))
            .unwrap_err()
            .category,
        "StaleIdentity"
    );
    lock(&host.inner.state).geometry += 1;
    assert_eq!(
        host.call("scan_ocr_zones", request(&observation))
            .unwrap_err()
            .category,
        "StaleIdentity"
    );
    lock(&host.inner.state).geometry -= 1;
    host.call("release", json!({"id":observation["id"]}))
        .unwrap();
    assert_eq!(
        host.call("scan_ocr_zones", request(&observation))
            .unwrap_err()
            .category,
        "InvalidHandle"
    );
    assert_eq!(host.snapshot()["recognitions"], 0);
    assert_eq!(host.finish()["clean"], true);
    assert_eq!(foreign.finish()["clean"], true);
}

#[test]
fn grouped_scan_output_overflow_keeps_only_the_callers_observation() {
    let host = ready_host("success");
    lock(&host.inner.state).visible = "x".repeat(crate::recognition_trial::DIAGNOSTIC_BYTES);
    let observation = observe(&host);
    assert_eq!(
        host.call("scan_ocr_zones", request(&observation))
            .unwrap_err()
            .category,
        "RecognitionOutputLimit"
    );
    assert_eq!(host.snapshot()["live_handles"], 1);
    host.call("release", json!({"id":observation["id"]}))
        .unwrap();
    assert_eq!(host.snapshot()["live_handles"], 0);
    assert_eq!(host.finish()["clean"], true);
}

#[cfg(feature = "engine")]
mod engine {
    use super::*;
    use mado_pilot as mp;

    // Only candidate production is controlled. The public pinned replay/session,
    // grouped membership, normalization, projection and lifetime paths are real.
    struct ScanBackend {
        descriptor: mp::OcrBackendDescriptor,
        calls: AtomicUsize,
        candidates: AtomicUsize,
        text: Mutex<Option<String>>,
        cancel_after: AtomicBool,
        control: Arc<Control>,
        mapped: Mutex<Option<(mp::PixelRect, Vec<mp::PixelRect>)>>,
    }

    impl std::fmt::Debug for ScanBackend {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter
                .debug_struct("ScanBackend")
                .finish_non_exhaustive()
        }
    }

    impl mp::OcrBackend for ScanBackend {
        fn descriptor(&self) -> mp::OcrBackendDescriptor {
            self.descriptor.clone()
        }

        fn recognize(
            &self,
            request: &mp::OcrBackendRequest<'_>,
            output: &mut dyn mp::OcrCandidateSink,
            operation: &mp::OperationContext,
        ) -> mp::Result<()> {
            self.calls.fetch_add(1, Ordering::AcqRel);
            *lock(&self.mapped) = Some((
                request.region(),
                request.interests().unwrap().zones().to_vec(),
            ));
            let text = lock(&self.text);
            for index in 0..self.candidates.load(Ordering::Acquire) {
                if let Some(interruption) = operation.interruption() {
                    return Err(interruption.into());
                }
                let (default, x, y) = match index % 3 {
                    0 => ("  e\u{301}  ", 0.5, 0.5),
                    1 => ("SECOND", 1.5, 2.0),
                    _ => ("right", 4.0, 0.5),
                };
                output.push(mp::BackendCandidate::new(
                    text.as_deref().unwrap_or(default).as_bytes(),
                    [(x, y), (x + 1.0, y), (x + 1.0, y + 1.0), (x, y + 1.0)],
                    0.9,
                    u32::try_from(index).unwrap(),
                ))?;
            }
            if self.cancel_after.load(Ordering::Acquire) {
                self.control.cancel();
            }
            Ok(())
        }

        fn close(&self, operation: &mp::OperationContext) -> mp::Result<()> {
            operation
                .interruption()
                .map_or(Ok(()), |interruption| Err(interruption.into()))
        }
    }

    fn replay_host() -> (Host, Arc<ScanBackend>) {
        let mut host = make_host("success", "template-first");
        let inner = Arc::get_mut(&mut host.inner).unwrap();
        inner.plan.lane = "replay".into();
        inner.plan.limits.handles = 1;
        inner.handle_budget = Arc::new(HandleBudget::new(1));
        let backend = Arc::new(ScanBackend {
            descriptor: mp::OcrBackendDescriptor::new(
                mp::OcrBackendIdentity::new(
                    mp::OcrBackendId::new("script-grouped-test").unwrap(),
                    mp::OcrBackendVersion::new("1").unwrap(),
                ),
                mp::OcrModelIdentity::accepted_g004(),
                mp::PixelFormat::Rgba8,
            ),
            calls: AtomicUsize::new(0),
            candidates: AtomicUsize::new(3),
            text: Mutex::new(None),
            cancel_after: AtomicBool::new(false),
            control: Arc::clone(&inner.control),
            mapped: Mutex::new(None),
        });
        inner.engine = Some(crate::engine::Engine::replay_with_ocr_for_test(
            &inner.plan,
            Arc::clone(&inner.control),
            &inner.lifetime,
            inner.attempt,
            Arc::clone(&inner.handle_budget),
            Some(backend.clone()),
        ));
        host.begin_readiness().unwrap();
        host.begin_workflow().unwrap();
        (host, backend)
    }

    fn content_request(observation: &Value) -> Value {
        let mut args = request(observation);
        args["basis"]["content"] = json!({"x":1,"y":2,"width":6,"height":4});
        args
    }

    fn assert_only_observation(host: &Host) {
        assert_eq!(host.snapshot()["live_handles"], 1);
        let engine = host.inner.engine.as_ref().unwrap();
        assert_eq!(engine.snapshot()["script_handles"], 1);
        assert_eq!(engine.snapshot()["in_flight"], 0);
    }

    #[test]
    fn grouped_scan_engine_maps_once_preserves_all_regions_and_caller_order() {
        let (host, backend) = replay_host();
        let observation = observe(&host);
        let mut args = content_request(&observation);
        args["zones"] = json!([
            zone("right-first", 0.5, 0.0, 1.0, 1.0),
            zone("left-second", 0.0, 0.0, 0.5, 1.0),
            zone("all", 0.0, 0.0, 1.0, 1.0),
            zone("empty", 0.5, 0.75, 1.0, 1.0)
        ]);
        let result = host.call("scan_ocr_zones", args).unwrap();
        assert_eq!(backend.calls.load(Ordering::Acquire), 1);
        let mapped = lock(&backend.mapped);
        let (envelope, interests) = mapped.as_ref().unwrap();
        assert_eq!(*envelope, mp::PixelRect::new(1, 2, 7, 6).unwrap());
        assert_eq!(
            interests,
            &vec![
                mp::PixelRect::new(3, 0, 6, 4).unwrap(),
                mp::PixelRect::new(0, 0, 3, 4).unwrap(),
                mp::PixelRect::new(0, 0, 6, 4).unwrap(),
                mp::PixelRect::new(3, 3, 6, 4).unwrap(),
            ]
        );
        drop(mapped);
        assert_eq!(result["observation"], observation);
        assert!(result.get("id").is_none());
        assert_eq!(
            result["zones"]
                .as_array()
                .unwrap()
                .iter()
                .map(|value| value["id"].as_str().unwrap())
                .collect::<Vec<_>>(),
            vec!["right-first", "left-second", "all", "empty"]
        );
        let texts = |index: usize| {
            result["zones"][index]["regions"]
                .as_array()
                .unwrap()
                .iter()
                .map(|value| value["text"].as_str().unwrap())
                .collect::<Vec<_>>()
        };
        assert_eq!(texts(0), vec!["right"]);
        assert_eq!(texts(1), vec!["é", "SECOND"]);
        assert_eq!(texts(2), vec!["é", "SECOND", "right"]);
        assert_eq!(
            result["zones"][3],
            json!({"id":"empty","outcome":"no_match","regions":[]})
        );
        assert_eq!(
            result["zones"][1]["regions"][0],
            json!({"text":"é","confidence":0.9,
            "bounds":{"x":1.5,"y":2.5,"width":1.0,"height":1.0},
            "geometry":[[1.5,2.5],[2.5,2.5],[2.5,3.5],[1.5,3.5]]})
        );
        assert_only_observation(&host);
        host.call("release", json!({"id":observation["id"]}))
            .unwrap();
        assert_eq!(host.finish()["clean"], true);
        assert_eq!(texts(1), vec!["é", "SECOND"]);
    }

    #[test]
    fn grouped_scan_engine_invalid_selection_or_observation_never_reaches_backend() {
        let (host, backend) = replay_host();
        let observation = observe(&host);
        let valid = content_request(&observation);
        for zones in [
            json!([]),
            Value::Array(
                (0..=mp::MAX_OCR_ZONES)
                    .map(|index| zone(&index.to_string(), 0.0, 0.0, 1.0, 1.0))
                    .collect(),
            ),
            json!([
                zone("first", 0.0, 0.0, 1.0, 1.0),
                zone("later-invalid", 0.0, 0.0, 1.1, 1.0)
            ]),
        ] {
            let mut invalid = valid.clone();
            invalid["zones"] = zones;
            assert_eq!(
                host.call("scan_ocr_zones", invalid).unwrap_err().category,
                "Argument"
            );
        }
        let mut changed = valid.clone();
        changed["basis"]["frame_width"] = json!(9);
        assert_eq!(
            host.call("scan_ocr_zones", changed).unwrap_err().category,
            "Argument"
        );
        let mut forged = valid.clone();
        forged["observation"]["epoch"] = json!(999);
        assert_eq!(
            host.call("scan_ocr_zones", forged).unwrap_err().category,
            "StaleIdentity"
        );
        let (foreign, foreign_backend) = replay_host();
        assert_eq!(
            foreign
                .call("scan_ocr_zones", valid.clone())
                .unwrap_err()
                .category,
            "InvalidHandle"
        );
        assert_eq!(foreign_backend.calls.load(Ordering::Acquire), 0);
        assert_eq!(foreign.finish()["clean"], true);
        host.call("release", json!({"id":observation["id"]}))
            .unwrap();
        assert_eq!(
            host.call("scan_ocr_zones", valid).unwrap_err().category,
            "InvalidHandle"
        );
        assert_eq!(backend.calls.load(Ordering::Acquire), 0);
        assert_eq!(host.finish()["clean"], true);
    }

    #[test]
    fn grouped_scan_engine_refuses_count_and_escaped_byte_overflow_without_retaining_results() {
        let (host, backend) = replay_host();
        let observation = observe(&host);
        let args = content_request(&observation);
        backend.candidates.store(
            crate::recognition_trial::DIAGNOSTIC_REGIONS,
            Ordering::Release,
        );
        let exact = host.call("scan_ocr_zones", args.clone()).unwrap();
        let regions = exact["zones"][0]["regions"].as_array().unwrap();
        assert_eq!(regions.len(), crate::recognition_trial::DIAGNOSTIC_REGIONS);
        assert_eq!(regions.last().unwrap()["text"], "é");
        assert_only_observation(&host);
        for (count, text) in [
            (crate::recognition_trial::DIAGNOSTIC_REGIONS + 1, None),
            (33, Some("\\".repeat(mp::MAX_TEXT_BYTES))),
        ] {
            backend.candidates.store(count, Ordering::Release);
            *lock(&backend.text) = text;
            assert_eq!(
                host.call("scan_ocr_zones", args.clone())
                    .unwrap_err()
                    .category,
                "RecognitionOutputLimit"
            );
            assert_only_observation(&host);
        }
        // Memberships repeated across overlapping zones count toward one total budget.
        backend.candidates.store(
            crate::recognition_trial::DIAGNOSTIC_REGIONS / mp::MAX_OCR_ZONES + 1,
            Ordering::Release,
        );
        *lock(&backend.text) = None;
        let mut overlapping = args.clone();
        overlapping["zones"] = Value::Array(
            (0..mp::MAX_OCR_ZONES)
                .map(|index| zone(&index.to_string(), 0.0, 0.0, 1.0, 1.0))
                .collect(),
        );
        assert_eq!(
            host.call("scan_ocr_zones", overlapping)
                .unwrap_err()
                .category,
            "RecognitionOutputLimit"
        );
        assert_only_observation(&host);
        backend.candidates.store(3, Ordering::Release);
        *lock(&backend.text) = None;
        let recovered = host.call("scan_ocr_zones", args).unwrap();
        assert_eq!(recovered["zones"][0]["regions"][2]["text"], "right");
        assert_only_observation(&host);
        host.call("release", json!({"id":observation["id"]}))
            .unwrap();
        assert_eq!(host.finish()["clean"], true);
    }

    #[test]
    fn grouped_scan_engine_discards_completion_after_cancellation_and_releases_owners() {
        let (host, backend) = replay_host();
        let observation = observe(&host);
        backend.cancel_after.store(true, Ordering::Release);
        assert_eq!(
            host.call("scan_ocr_zones", content_request(&observation))
                .unwrap_err()
                .category,
            "Cancelled"
        );
        assert_eq!(backend.calls.load(Ordering::Acquire), 1);
        assert_only_observation(&host);
        host.call("release", json!({"id":observation["id"]}))
            .unwrap();
        assert_eq!(host.snapshot()["live_handles"], 0);
        assert_eq!(host.finish()["clean"], true);
    }
}
