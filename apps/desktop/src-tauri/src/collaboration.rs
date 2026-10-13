//! App-owned private local authoring transport.
//!
//! A frame is a 4-byte unsigned big-endian length followed by that many bytes
//! of UTF-8 JSON, exchanged over a user-private Unix-domain socket or Windows
//! local named pipe advertised by `<configured-data-root>/collaboration/<instance>.json`.
//! The host does not interpret drafts: an authenticated request becomes a ticket that only the
//! main window may claim and answer from its live authoring controller.
//!
//! Before claim, cancellation, disconnect, expiry, unavailability or shutdown
//! withdraws a request with outcome `not_applied`. After claim, a missing reply
//! is `unknown`, never a rollback, and nothing is replayed.

use mado_runtime_comparison::model::Fault;
use serde::Serialize;
use serde_json::Value;
use std::path::PathBuf;
use std::sync::Arc;
#[cfg(any(unix, windows))]
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

// Only `Claimed` is reachable without a supported local transport.
#[cfg_attr(not(any(unix, windows)), allow(dead_code))]
mod protocol;
pub use protocol::Claimed;
#[cfg(unix)]
mod endpoint;
#[cfg(windows)]
#[path = "collaboration/windows.rs"]
mod endpoint;
#[cfg(all(test, any(unix, windows)))]
mod tests;
#[cfg(any(unix, windows))]
mod transport;

pub const PROTOCOL: u32 = 1;
pub const FRAME_BYTES: usize = 8_388_608;
pub const MAX_CLIENTS: usize = 4;
pub const MAX_PENDING: usize = 16;
/// Per-client queued replies, including one admission refusal.
pub const OUTBOX_FRAMES: usize = MAX_PENDING + 1;
/// Retained request frames never exceed this aggregate.
pub const PENDING_BYTES: usize = FRAME_BYTES * MAX_PENDING;
pub const PAGE_UNITS: u32 = 65_536;
pub const RESPONSE_TIMEOUT: Duration = Duration::from_secs(30);

/// Announces a ticket to the main window; `false` means it cannot be delivered.
pub type Notify = Arc<dyn Fn(&str) -> bool + Send + Sync>;
/// The host's current Edit owner token, if a lease exists.
pub type HostOwner = Arc<dyn Fn() -> Option<String> + Send + Sync>;

#[derive(Debug, Serialize)]
pub struct Ready {
    pub instance: String,
    pub protocol: u32,
}

pub struct Collaboration {
    #[cfg(any(unix, windows))]
    root: Option<PathBuf>,
    #[cfg(any(unix, windows))]
    hooks: transport::Hooks,
    #[cfg(any(unix, windows))]
    settings: transport::Settings,
    #[cfg(any(unix, windows))]
    slot: Mutex<Slot>,
}

#[cfg(any(unix, windows))]
struct Slot {
    server: Option<Arc<transport::Server>>,
    closed: bool,
}

#[cfg(any(unix, windows))]
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn stale_ticket() -> Fault {
    Fault::new(
        "CollaborationTicket",
        "The collaboration request is no longer awaiting this reply",
    )
}

impl Collaboration {
    /// `root` is the configured data root; it is never created here.
    pub fn new(root: Option<PathBuf>, notify: Notify, host_owner: HostOwner) -> Self {
        #[cfg(any(unix, windows))]
        {
            Self::with_settings(root, notify, host_owner, transport::Settings::default())
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = (root, notify, host_owner);
            Self {}
        }
    }

    #[cfg(any(unix, windows))]
    fn with_settings(
        root: Option<PathBuf>,
        notify: Notify,
        host_owner: HostOwner,
        settings: transport::Settings,
    ) -> Self {
        Self {
            root,
            hooks: transport::Hooks { notify, host_owner },
            settings,
            slot: Mutex::new(Slot {
                server: None,
                closed: false,
            }),
        }
    }

    /// Starts the endpoint, or attaches to the running one, and marks the
    /// main-window controller available. Call only once configuration is ready.
    ///
    /// # Errors
    ///
    /// `Closing` after shutdown began; `CollaborationUnavailable` when the
    /// platform has no supported local transport or the private endpoint cannot
    /// be created safely. Neither affects ordinary authoring.
    pub fn ready(&self) -> Result<Ready, Fault> {
        #[cfg(any(unix, windows))]
        {
            let mut slot = lock(&self.slot);
            if slot.closed {
                return Err(Fault::new("Closing", "Application is closing"));
            }
            let server = if let Some(server) = &slot.server {
                server.clone()
            } else {
                let root = self.root.as_deref().ok_or_else(|| {
                    Fault::new(
                        "CollaborationUnavailable",
                        "No configured data root is available",
                    )
                })?;
                let server = Arc::new(transport::Server::start(
                    root,
                    self.hooks.clone(),
                    self.settings,
                )?);
                slot.server = Some(server.clone());
                server
            };
            server.register();
            Ok(Ready {
                instance: server.instance().to_owned(),
                protocol: PROTOCOL,
            })
        }
        #[cfg(not(any(unix, windows)))]
        {
            Err(Fault::new(
                "CollaborationUnavailable",
                "This platform build does not provide a supported private local transport",
            ))
        }
    }

    /// The main-window controller is gone: unclaimed work is `not_applied`,
    /// claimed work is `unknown`, and nothing is dispatched until `ready`.
    pub fn unavailable(&self) {
        #[cfg(any(unix, windows))]
        {
            if let Some(server) = self.server() {
                server.unregister();
            }
        }
    }

    /// Atomically dispatches an unexpired, uncancelled request. Read and edit
    /// also require the request owner to be the host's current Edit owner.
    pub fn claim(&self, ticket: &str) -> Option<Claimed> {
        #[cfg(any(unix, windows))]
        {
            self.server()?.claim(ticket)
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = ticket;
            None
        }
    }

    /// Delivers the controller's reply for a claimed ticket.
    ///
    /// # Errors
    ///
    /// `CollaborationTicket` when the ticket is not awaiting a reply (its client
    /// already received an `unknown` outcome); `CollaborationReply` when the
    /// reply is malformed (its client receives `invalid_reply`, `unknown`).
    pub fn reply(&self, ticket: &str, response: Value) -> Result<(), Fault> {
        #[cfg(any(unix, windows))]
        {
            self.server()
                .ok_or_else(stale_ticket)?
                .reply(ticket, response)
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = (ticket, response);
            Err(stale_ticket())
        }
    }

    /// Closes admission, withdraws the advertisement, waits up to `drain` for
    /// claimed replies, then reports the rest as `unknown`. Idempotent; it does
    /// not touch authoring leases or child cleanup.
    pub fn shutdown(&self, drain: Duration) {
        #[cfg(any(unix, windows))]
        {
            let server = {
                let mut slot = lock(&self.slot);
                slot.closed = true;
                slot.server.clone()
            };
            if let Some(server) = server {
                server.shutdown(drain);
            }
        }
        #[cfg(not(any(unix, windows)))]
        let _ = drain;
    }

    #[cfg(any(unix, windows))]
    fn server(&self) -> Option<Arc<transport::Server>> {
        lock(&self.slot).server.clone()
    }
}
