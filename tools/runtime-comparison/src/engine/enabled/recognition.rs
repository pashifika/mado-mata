use super::*;
use std::sync::mpsc::{self, Receiver, SyncSender};

pub(super) type Recognized = Option<(Managed<Retained>, Value)>;

pub(super) struct Job {
    pub resources: Arc<Resources>,
    pub session: Arc<mp::Session>,
    pub frame: mp::Frame,
    pub request: RecognitionRequest,
    pub operation: mp::OperationContext,
    pub text_limit: usize,
    pub permit: HandlePermit,
}

struct ActiveJob {
    busy: Arc<AtomicBool>,
    in_flight: Arc<AtomicUsize>,
}

impl Drop for ActiveJob {
    fn drop(&mut self) {
        self.in_flight.fetch_sub(1, Ordering::AcqRel);
        self.busy.store(false, Ordering::Release);
    }
}

struct Message {
    job: Job,
    reply: SyncSender<Result<Recognized, Fault>>,
    _active: ActiveJob,
}

// One worker and one outstanding recognition per attempt, including timed-out work.
// The worker never owns the engine state or publishes script handles.
pub(super) struct Worker {
    sender: Option<SyncSender<Message>>,
    thread: Option<JoinHandle<()>>,
    busy: Arc<AtomicBool>,
    failed: bool,
}

impl Worker {
    pub fn new() -> Result<Self, Fault> {
        let (sender, receiver) = mpsc::sync_channel::<Message>(1);
        let thread = thread::Builder::new()
            .name("query-recognition".into())
            .spawn(move || {
                while let Ok(message) = receiver.recv() {
                    let Job {
                        resources,
                        session,
                        frame,
                        request,
                        operation,
                        text_limit,
                        permit,
                    } = message.job;
                    let result = recognize_frame(
                        &resources, &session, &frame, &request, &operation, text_limit,
                    )
                    .map(|found| {
                        found.map(|(retained, value)| (Managed::new(retained, permit), value))
                    });
                    // An expired caller drops its receiver; the late result and permit die here.
                    let _ = message.reply.send(result);
                }
            })
            .map_err(|error| Fault::new("Initialization", error.to_string()))?;
        Ok(Self {
            sender: Some(sender),
            thread: Some(thread),
            busy: Arc::new(AtomicBool::new(false)),
            failed: false,
        })
    }

    pub fn is_busy(&self) -> bool {
        self.busy.load(Ordering::Acquire)
    }

    pub fn submit(
        &self,
        job: Job,
        in_flight: Arc<AtomicUsize>,
    ) -> Result<Receiver<Result<Recognized, Fault>>, Fault> {
        if self.busy.swap(true, Ordering::AcqRel) {
            return Err(Fault::new(
                "ResourceBusy",
                "query recognition is still in flight",
            ));
        }
        in_flight.fetch_add(1, Ordering::AcqRel);
        let active = ActiveJob {
            busy: Arc::clone(&self.busy),
            in_flight,
        };
        let (reply, receiver) = mpsc::sync_channel(1);
        self.sender
            .as_ref()
            .ok_or_else(|| internal("query worker is closed"))?
            .send(Message {
                job,
                reply,
                _active: active,
            })
            .map_err(|_| Fault::new("Backend", "query recognition worker stopped"))?;
        Ok(receiver)
    }

    pub fn settle(&mut self, deadline: Instant) -> Result<Option<Fault>, Fault> {
        self.sender.take();
        while self
            .thread
            .as_ref()
            .is_some_and(|worker| !worker.is_finished())
        {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(Fault::new(
                    "IncompleteCleanup",
                    "query recognition is still in flight",
                ));
            }
            thread::sleep(remaining.min(Duration::from_millis(1)));
        }
        if let Some(worker) = self.thread.take() {
            self.failed = worker.join().is_err();
        }
        Ok(self
            .failed
            .then(|| Fault::new("Backend", "query recognition worker panicked")))
    }
}

pub(super) fn recognize_frame(
    resources: &Resources,
    session: &mp::Session,
    frame: &mp::Frame,
    request: &RecognitionRequest,
    operation: &mp::OperationContext,
    text_limit: usize,
) -> Result<Option<(Retained, Value)>, Fault> {
    let roi = request.roi.rect(frame)?;
    match request.kind.as_str() {
        "template" => {
            let alias = request
                .asset
                .as_ref()
                .ok_or_else(|| argument("template asset is required"))?;
            let template = resources.templates.get(alias).ok_or_else(|| {
                argument("template asset is not declared in engine configuration")
            })?;
            let view = frame
                .view(roi, mp::ClipPolicy::Reject)
                .map_err(|error| engine_error("template_roi", error.into()))?;
            let find = mp::FindRequest::view(
                &view,
                template,
                mp::MatchOptions::from_defaults(template.defaults()),
            )
            .map_err(|error| engine_error("template_request", error))?;
            let outcome = session
                .find_template(&find, operation)
                .map_err(|error| engine_error("template_recognition", error))?;
            let Some(found) = outcome.result().matches().first() else {
                return Ok(None);
            };
            let bounds = found.bounds();
            let value = json!({"kind":"template","observation":request.observation,
                "region":{"x":bounds.left(),"y":bounds.top(),"width":bounds.width(),"height":bounds.height()},
                "score":found.score()});
            Ok(Some((Retained::Template(outcome), value)))
        }
        "ocr" => {
            if request.asset.is_some() {
                return Err(argument("OCR forbids asset"));
            }
            let backend = resources
                .engine
                .ocr_backend()
                .ok_or_else(|| internal("OCR backend missing"))?;
            let outcome = session
                .recognize(mp::OcrRequest::new(
                    frame,
                    backend.backend_identity(),
                    backend.model_identity(),
                    mp::OcrRegion::Region {
                        rect: roi,
                        policy: mp::ClipPolicy::Reject,
                    },
                    mp::CoordinateSpace::CapturePixels,
                    operation,
                ))
                .map_err(|error| engine_error("ocr_recognition", error))?;
            let found = outcome.regions().iter().find(|region| {
                request
                    .expected
                    .as_ref()
                    .is_none_or(|text| region.text() == text)
            });
            let Some(found) = found else {
                return Ok(None);
            };
            if found.text().len() > text_limit {
                return Err(Fault::new(
                    "LimitExceeded",
                    "OCR compact text exceeds configured byte bound",
                ));
            }
            let points = found.geometry().points();
            let left = points
                .iter()
                .map(|point| point.x())
                .fold(f64::INFINITY, f64::min);
            let top = points
                .iter()
                .map(|point| point.y())
                .fold(f64::INFINITY, f64::min);
            let right = points
                .iter()
                .map(|point| point.x())
                .fold(f64::NEG_INFINITY, f64::max);
            let bottom = points
                .iter()
                .map(|point| point.y())
                .fold(f64::NEG_INFINITY, f64::max);
            let value = json!({"kind":"ocr","observation":request.observation,
                "region":{"x":left,"y":top,"width":right-left,"height":bottom-top},
                "score":found.confidence().get(),"text":found.text()});
            Ok(Some((Retained::Ocr(outcome), value)))
        }
        _ => Err(argument("unknown recognition kind")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_joined_worker_failure_does_not_prevent_session_cleanup() {
        let plan: Plan =
            serde_json::from_str(include_str!("../../../fixtures/manual-plan.json")).unwrap();
        let engine = Engine::replay_for_test(
            &plan,
            Arc::new(Control::new(&plan.limits)),
            "worker-failure",
            1,
            Arc::new(HandleBudget::new(plan.limits.handles)),
        );
        engine.call("observe", json!({})).unwrap();
        assert!(engine.handle_budget.reserve(engine.limits.handles).is_err());
        *engine.query_worker.lock().unwrap() = Some(Worker {
            sender: None,
            thread: Some(thread::spawn(|| {
                panic!("injected recognition worker failure")
            })),
            busy: Arc::new(AtomicBool::new(false)),
            failed: false,
        });
        let cleanup = engine.finish();
        assert_eq!(cleanup["worker_error"]["category"], "Backend");
        assert_eq!(cleanup["session_closed"], true);
        assert_eq!(cleanup["physical_cleanup_confirmed"], true);
        assert!(engine.handle_budget.reserve(engine.limits.handles).is_ok());
        assert_eq!(engine.finish(), cleanup);
    }
}
