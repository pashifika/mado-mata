use super::*;

const WIDTH: u32 = 16;
const HEIGHT: u32 = 12;
const PATCH: [u8; 16] = [
    0, 255, 0, 255, 255, 0, 255, 0, 0, 0, 255, 255, 255, 255, 0, 0,
];

fn plant(pixels: &mut [u8], x: usize, y: usize, patch: &[u8]) {
    for row in 0..4 {
        let start = (y + row) * WIDTH as usize + x;
        pixels[start..start + 4].copy_from_slice(&patch[row * 4..row * 4 + 4]);
    }
}

fn replay_session(frames: Vec<mp::replay::ReplayFrame>) -> (mp::Engine, mp::Session) {
    let source = mp::replay::ReplaySource::from_targets(vec![
        mp::replay::ReplayTarget::new("template-matching", frames).unwrap(),
    ])
    .unwrap();
    let facade = mp::replay_engine(mp::ReplayEngineRequest::new(source)).unwrap();
    let operation = mp::OperationContext::new();
    let targets = facade.discover(&operation).unwrap();
    let session = facade
        .open(targets[0].id(), &mp::OpenRequest::new(), &operation)
        .unwrap();
    (facade, session)
}

fn prepare_png(
    facade: &mp::Engine,
    extent: mp::PixelExtent,
    color: png::ColorType,
    pixels: &[u8],
) -> mp::PreparedTemplate {
    let mut png = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut png, extent.width(), extent.height());
        encoder.set_color(color);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(pixels).unwrap();
        writer.finish().unwrap();
    }
    let source = mp::TemplateSource::new(mp::TemplateSourceRequest {
        id: mp::TemplateId::new("reviewed").unwrap(),
        encoding: mp::TemplateEncoding::Png,
        extent,
        space: mp::CoordinateSpace::CapturePixels,
        defaults: mp::MatchDefaults::new(0.9, 8).unwrap(),
        content: Arc::from(png),
    })
    .unwrap();
    facade
        .prepare_template(&source, &mp::OperationContext::new())
        .unwrap()
}

fn replay(frames: Vec<Vec<u8>>, patch: &[u8]) -> Engine {
    let mut plan: Plan =
        serde_json::from_str(include_str!("../../../fixtures/manual-plan.json")).unwrap();
    plan.lane = "replay".into();
    plan.limits.wait_ms = 5_000;
    let mut engine = Engine::replay_for_test(
        &plan,
        Arc::new(Control::new(&plan.limits)),
        "template-test",
        1,
        Arc::new(HandleBudget::new(plan.limits.handles)),
    );
    let descriptor =
        mp::FrameDescriptor::packed(mp::PixelExtent::new(WIDTH, HEIGHT), mp::PixelFormat::Rgba8)
            .unwrap();
    let frames = frames
        .into_iter()
        .enumerate()
        .map(|(index, pixels)| {
            assert_eq!(pixels.len(), (WIDTH * HEIGHT) as usize);
            let rgba = pixels
                .into_iter()
                .flat_map(|value| [value, value, value, 255])
                .collect::<Vec<_>>();
            mp::replay::ReplayFrame::new(
                descriptor,
                mp::MonotonicInstant::from_origin(Duration::from_nanos(index as u64)),
                mp::Continuity::Continuous,
                None,
                rgba.into_boxed_slice(),
            )
            .unwrap()
        })
        .collect();
    let (facade, session) = replay_session(frames);
    let operation = mp::OperationContext::new();
    let template = prepare_png(
        &facade,
        mp::PixelExtent::new(4, 4),
        png::ColorType::Grayscale,
        patch,
    );
    {
        let mut state = engine.lock().unwrap();
        state.session.take().unwrap().close(&operation).unwrap();
        state.session = Some(Arc::new(session));
    }
    let resources = Arc::get_mut(&mut engine.resources).unwrap();
    resources.engine = facade;
    resources.templates.insert("reviewed".into(), template);
    engine
}

fn request(observation: &Value, roi: [u32; 4]) -> Value {
    let [x, y, width, height] = roi;
    json!({"kind":"template","asset":"reviewed","observation":observation,
        "roi":{"x":x,"y":y,"width":width,"height":height}})
}

fn reference(
    engine: &Engine,
    observation: &Value,
    roi: [u32; 4],
    suppression: mp::Suppression,
) -> mp::FindOutcome {
    let state = engine.lock().unwrap();
    let frame = &engine.observation(&state, observation).unwrap().frame;
    let [x, y, width, height] = roi;
    let view = frame
        .view(
            mp::Rect::from_origin_size(
                mp::CoordinateSpace::CapturePixels,
                f64::from(x),
                f64::from(y),
                f64::from(width),
                f64::from(height),
            )
            .unwrap(),
            mp::ClipPolicy::Reject,
        )
        .unwrap();
    let template = &engine.resources.templates["reviewed"];
    let find = mp::FindRequest::view(
        &view,
        template,
        mp::MatchOptions::from_defaults(template.defaults()).with_suppression(suppression),
    )
    .unwrap();
    state
        .session
        .as_ref()
        .unwrap()
        .find_template(&find, &mp::OperationContext::new())
        .unwrap()
}

fn bounds(outcome: &mp::FindOutcome) -> Vec<[i64; 4]> {
    outcome
        .result()
        .matches()
        .iter()
        .map(|found| {
            let rect = found.bounds();
            [
                i64::from(rect.left()),
                i64::from(rect.top()),
                i64::from(rect.width()),
                i64::from(rect.height()),
            ]
        })
        .collect()
}

fn assert_match(result: &Value, observation: &Value, expected: [i64; 4], score: f64) {
    let [x, y, width, height] = expected;
    assert_eq!(result["kind"], "template");
    assert!(result["id"].as_str().is_some());
    assert_eq!(result["observation"], *observation);
    assert_eq!(
        result["region"],
        json!({"x":x,"y":y,"width":width,"height":height})
    );
    assert_eq!(result["score"].as_f64().unwrap(), score);
    assert!((score - 1.0).abs() < 1e-6);
}

fn release(engine: &Engine, handle: &Value) {
    assert_eq!(
        engine.call("release", json!({"id":handle["id"]})).unwrap(),
        json!({"released":true})
    );
}

fn assert_first(engine: &Engine, observation: &Value, roi: [u32; 4], expected: [i64; 4]) {
    let reference = reference(engine, observation, roi, mp::Suppression::DropOverlapping);
    assert_eq!(bounds(&reference)[0], expected);
    assert_eq!(
        reference.result().stamp().sequence().value(),
        observation["frame"].as_u64().unwrap()
    );
    let result = engine.call("recognize", request(observation, roi)).unwrap();
    assert_match(
        &result,
        observation,
        expected,
        reference.result().matches()[0].score(),
    );
    release(engine, &result);

    let query = engine.call("query", request(observation, roi)).unwrap();
    let wait = json!({"id":query["id"],"timeout_ms":5_000});
    let found = engine.call("query_wait", wait.clone()).unwrap();
    let retained = &found["observation"];
    assert_ne!(retained["id"], observation["id"]);
    let mut same_source = retained.clone();
    same_source["id"] = observation["id"].clone();
    assert_eq!(same_source, *observation);
    assert_match(
        &found,
        retained,
        expected,
        reference.result().matches()[0].score(),
    );
    assert_eq!(engine.call("query_wait", wait).unwrap(), found);
    release(engine, &query);
    // Releasing a terminal query does not release its published result or frame.
    assert_eq!(
        engine
            .call("validate_observation", json!({"observation":retained}))
            .unwrap(),
        json!({"valid":true})
    );
    release(engine, &found);
    release(engine, retained);
}

fn finish(engine: &Engine) {
    assert_eq!(engine.snapshot()["script_handles"], 0);
    assert!(engine.handle_budget.reserve(engine.limits.handles).is_ok());
    assert_eq!(engine.finish()["clean"], true);
}

#[test]
fn observation_crop_finds_moved_patch_on_later_replay_frame() {
    const R: [u8; 4] = [240, 10, 30, 255];
    const G: [u8; 4] = [20, 210, 40, 255];
    const B: [u8; 4] = [15, 35, 220, 255];
    const ART: [[u8; 4]; 12] = [R, G, B, R, B, R, G, G, G, B, B, R];
    let descriptor = mp::FrameDescriptor::new(
        mp::PixelExtent::new(WIDTH, HEIGHT),
        mp::PixelFormat::Bgra8,
        72,
    )
    .unwrap();
    let frame_at = |x: usize, y: usize, time| {
        let mut bytes = vec![255; descriptor.byte_len()];
        for (index, &[r, g, b, a]) in ART.iter().enumerate() {
            let start = (y + index / 4) * descriptor.stride() + (x + index % 4) * 4;
            bytes[start..start + 4].copy_from_slice(&[b, g, r, a]);
        }
        mp::replay::ReplayFrame::new(
            descriptor,
            mp::MonotonicInstant::from_origin(Duration::from_nanos(time)),
            mp::Continuity::Continuous,
            None,
            bytes.into_boxed_slice(),
        )
        .unwrap()
    };
    let (facade, session) = replay_session(vec![frame_at(2, 1, 0), frame_at(10, 7, 1)]);
    let operation = mp::OperationContext::new();
    let rect = |x, y, w, h| {
        mp::Rect::from_origin_size(mp::CoordinateSpace::CapturePixels, x, y, w, h).unwrap()
    };
    let (stamp, template) = {
        let frame = session
            .acquire_frame(&mp::FrameRequest::latest(), &operation)
            .unwrap();
        let crop = session
            .map_region(
                &frame,
                rect(2., 1., 4., 3.),
                mp::ClipPolicy::Reject,
                mp::PixelFormat::Rgba8,
                &operation,
            )
            .unwrap();
        let extent = crop.descriptor().extent();
        assert_eq!(extent, mp::PixelExtent::new(4, 3));
        assert_eq!(crop.bytes(), ART.as_flattened());
        (
            frame.stamp(),
            prepare_png(&facade, extent, png::ColorType::Rgba, crop.bytes()),
        )
    };
    let later = session
        .acquire_frame(&mp::FrameRequest::newer_than(stamp), &operation)
        .unwrap();
    let view = later
        .view(rect(6., 4., 10., 8.), mp::ClipPolicy::Reject)
        .unwrap();
    let request = mp::FindRequest::view(
        &view,
        &template,
        mp::MatchOptions::from_defaults(template.defaults()),
    )
    .unwrap();
    let outcome = session.find_template(&request, &operation).unwrap();
    assert_eq!(outcome.result().stamp(), later.stamp());
    assert_eq!(bounds(&outcome), [[10, 7, 4, 3]]);
    session.close(&operation).unwrap();
}

#[test]
fn unique_best_template_beats_an_earlier_lower_scoring_candidate() {
    let mut pixels = vec![64; (WIDTH * HEIGHT) as usize];
    let mut near = PATCH;
    near[0] = 128;
    plant(&mut pixels, 1, 1, &near);
    plant(&mut pixels, 10, 6, &PATCH);
    let engine = replay(vec![pixels], &PATCH);
    let observation = engine.call("observe", json!({})).unwrap();
    let roi = [0, 0, WIDTH, HEIGHT];
    let reference = reference(&engine, &observation, roi, mp::Suppression::DropOverlapping);
    assert_eq!(bounds(&reference)[0], [10, 6, 4, 4]);
    let near = reference
        .result()
        .matches()
        .iter()
        .find(|found| found.bounds().left() == 1 && found.bounds().top() == 1)
        .unwrap();
    assert!(near.score() >= 0.9);
    assert!(near.score() < reference.result().matches()[0].score());
    assert_first(&engine, &observation, roi, [10, 6, 4, 4]);
    release(&engine, &observation);
    finish(&engine);
}

fn stripes() -> (Vec<u8>, Vec<u8>) {
    let row = |width| (0..width).map(|x| if x % 2 == 0 { 0 } else { 255 });
    let pixels = (0..HEIGHT).flat_map(|_| row(WIDTH)).collect();
    let patch = (0..4).flat_map(|_| row(4)).collect();
    (pixels, patch)
}

#[test]
fn exactly_tied_template_maxima_keep_canonical_row_major_first() {
    let (pixels, patch) = stripes();
    let engine = replay(vec![pixels], &patch);
    let observation = engine.call("observe", json!({})).unwrap();
    let roi = [0, 0, WIDTH, HEIGHT];
    let reference = reference(&engine, &observation, roi, mp::Suppression::DropOverlapping);
    assert_eq!(
        bounds(&reference),
        vec![
            [0, 0, 4, 4],
            [4, 0, 4, 4],
            [8, 0, 4, 4],
            [12, 0, 4, 4],
            [0, 4, 4, 4],
            [4, 4, 4, 4],
            [8, 4, 4, 4],
            [12, 4, 4, 4],
        ]
    );
    assert!(
        reference
            .result()
            .matches()
            .iter()
            .all(|found| found.score() == 1.0)
    );
    assert_first(&engine, &observation, roi, [0, 0, 4, 4]);
    release(&engine, &observation);
    finish(&engine);
}

#[test]
fn overlapping_and_separate_template_candidates_preserve_roi_offset_first() {
    let (pixels, patch) = stripes();
    let engine = replay(vec![pixels], &patch);
    let observation = engine.call("observe", json!({})).unwrap();
    let roi = [2, 3, 12, 4];
    let overlapping = reference(&engine, &observation, roi, mp::Suppression::KeepAll);
    assert_eq!(
        bounds(&overlapping),
        vec![
            [2, 3, 4, 4],
            [4, 3, 4, 4],
            [6, 3, 4, 4],
            [8, 3, 4, 4],
            [10, 3, 4, 4]
        ]
    );
    let separate = reference(&engine, &observation, roi, mp::Suppression::DropOverlapping);
    assert_eq!(
        bounds(&separate),
        vec![[2, 3, 4, 4], [6, 3, 4, 4], [10, 3, 4, 4]]
    );
    assert_eq!(
        overlapping.result().matches()[0].score(),
        separate.result().matches()[0].score()
    );
    assert_first(&engine, &observation, roi, [2, 3, 4, 4]);
    release(&engine, &observation);
    finish(&engine);
}

#[test]
fn template_no_match_stays_null_and_query_wait_uses_the_next_matching_frame() {
    let empty = vec![64; (WIDTH * HEIGHT) as usize];
    let mut matched = empty.clone();
    plant(&mut matched, 7, 5, &PATCH);
    let engine = replay(vec![empty, matched], &PATCH);
    let observation = engine.call("observe", json!({})).unwrap();
    let roi = [0, 0, WIDTH, HEIGHT];
    let reference = reference(&engine, &observation, roi, mp::Suppression::DropOverlapping);
    assert!(reference.result().matches().is_empty());
    assert!(
        engine
            .call("recognize", request(&observation, roi))
            .unwrap()
            .is_null()
    );
    let query = engine.call("query", request(&observation, roi)).unwrap();
    release(&engine, &observation);
    let wait = json!({"id":query["id"],"timeout_ms":5_000});
    let result = engine.call("query_wait", wait.clone()).unwrap();
    let source = &result["observation"];
    assert!(source["frame"].as_u64().unwrap() > observation["frame"].as_u64().unwrap());
    assert_eq!(source["session"], observation["session"]);
    assert_eq!(source["geometry"], observation["geometry"]);
    assert_match(
        &result,
        source,
        [7, 5, 4, 4],
        result["score"].as_f64().unwrap(),
    );
    assert_eq!(engine.call("query_wait", wait.clone()).unwrap(), result);
    release(&engine, &result);
    assert_eq!(
        engine.call("query_wait", wait).unwrap_err().category,
        "InvalidHandle"
    );
    release(&engine, &query);
    release(&engine, source);
    finish(&engine);
}

#[test]
fn template_query_owns_its_original_frame_and_rejects_released_or_changed_sources() {
    let mut first = vec![64; (WIDTH * HEIGHT) as usize];
    plant(&mut first, 3, 2, &PATCH);
    let mut second = vec![64; (WIDTH * HEIGHT) as usize];
    plant(&mut second, 9, 7, &PATCH);
    let engine = replay(vec![first, second], &PATCH);
    let observation = engine.call("observe", json!({})).unwrap();
    let roi = [0, 0, WIDTH, HEIGHT];
    let mut changed = observation.clone();
    changed["frame"] = json!(observation["frame"].as_u64().unwrap() + 1);
    assert_eq!(
        engine
            .call("recognize", request(&changed, roi))
            .unwrap_err()
            .category,
        "StaleIdentity"
    );
    let query = engine.call("query", request(&observation, roi)).unwrap();
    release(&engine, &observation);
    assert_eq!(
        engine
            .call("recognize", request(&observation, roi))
            .unwrap_err()
            .category,
        "InvalidHandle"
    );
    let newer = engine.call("observe", json!({})).unwrap();
    assert_ne!(newer["frame"], observation["frame"]);
    let wait = json!({"id":query["id"],"timeout_ms":5_000});
    let result = engine.call("query_wait", wait.clone()).unwrap();
    assert_eq!(result["observation"]["frame"], observation["frame"]);
    assert_match(
        &result,
        &result["observation"],
        [3, 2, 4, 4],
        result["score"].as_f64().unwrap(),
    );
    assert_eq!(engine.call("query_wait", wait.clone()).unwrap(), result);
    release(&engine, &result["observation"]);
    assert_eq!(
        engine
            .call("query_wait", wait.clone())
            .unwrap_err()
            .category,
        "InvalidHandle"
    );
    release(&engine, &result);
    release(&engine, &query);
    assert_eq!(
        engine.call("query_wait", wait).unwrap_err().category,
        "InvalidHandle"
    );
    release(&engine, &newer);
    finish(&engine);
}
