use crate::model::Fault;
use serde::Serialize;
use serde_json::json;

type Started = (u64, u64);

#[derive(Clone, Copy, Debug)]
pub(super) enum ProcessState {
    Alive(Started),
    Absent,
    Exited(Started),
}

#[cfg(all(test, feature = "engine", target_os = "macos"))]
pub(super) type ScriptedProbe = dyn Fn(i32) -> Result<ProcessState, Fault> + Send + Sync;

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
enum ExitReason {
    Absent,
    ReusedPid,
    Zombie,
}

#[cfg(target_os = "macos")]
fn unavailable(message: &str) -> Fault {
    Fault::new("Blocked", message).with_context(json!({"stage":"target_identity_unavailable"}))
}

pub(super) fn require_current(
    expected: Option<Started>,
    current: ProcessState,
) -> Result<Started, Fault> {
    let reason = match (expected, current) {
        (None, ProcessState::Alive(started)) => return Ok(started),
        (Some(expected), ProcessState::Alive(started)) if expected == started => {
            return Ok(started);
        }
        (Some(_), ProcessState::Alive(_)) => ExitReason::ReusedPid,
        (Some(_), ProcessState::Absent) => ExitReason::Absent,
        (Some(expected), ProcessState::Exited(started)) if expected == started => {
            ExitReason::Zombie
        }
        (Some(_), ProcessState::Exited(_)) => ExitReason::ReusedPid,
        (None, _) => {
            return Err(Fault::new(
                "TargetLost",
                "process ended before lifetime binding",
            ));
        }
    };
    Err(
        Fault::new("TargetExited", "the bound process lifetime ended")
            .with_context(json!({"stage":"target_process_exit","exit_reason":reason})),
    )
}

#[cfg(target_os = "macos")]
pub(super) fn probe(pid: i32) -> Result<ProcessState, Fault> {
    if pid <= 0 {
        return Err(unavailable("selected process identifier is invalid"));
    }
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::uninit();
    let size = i32::try_from(std::mem::size_of::<libc::proc_bsdinfo>())
        .map_err(|_| unavailable("process description size is not representable"))?;
    // SAFETY: libproc writes synchronously to aligned storage of the exact SDK size.
    // __error returns this thread's errno slot; clearing it excludes stale ESRCH.
    #[expect(unsafe_code, reason = "audited libproc buffer and thread-local errno")]
    let (returned, errno) = unsafe {
        *libc::__error() = 0;
        let returned = libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            size,
        );
        (returned, *libc::__error())
    };
    if returned != size {
        return classify_failure(returned, errno);
    }
    // SAFETY: libproc reported a complete initialized proc_bsdinfo.
    #[expect(
        unsafe_code,
        reason = "complete libproc output checked before initialization"
    )]
    let info = unsafe { info.assume_init() };
    classify_metadata(
        pid as u32,
        info.pbi_pid,
        info.pbi_status,
        (info.pbi_start_tvsec, info.pbi_start_tvusec),
    )
}

#[cfg(target_os = "macos")]
fn classify_failure(returned: i32, errno: i32) -> Result<ProcessState, Fault> {
    if returned <= 0 && errno == libc::ESRCH {
        Ok(ProcessState::Absent)
    } else {
        Err(unavailable(
            "kernel process identity lookup failed or returned incomplete data",
        ))
    }
}

#[cfg(target_os = "macos")]
pub(super) fn classify_metadata(
    pid: u32,
    actual_pid: u32,
    status: u32,
    started: Started,
) -> Result<ProcessState, Fault> {
    if actual_pid != pid || started.0 == 0 || started.1 >= 1_000_000 {
        return Err(unavailable("kernel process identity is unverifiable"));
    }
    Ok(if status == libc::SZOMB {
        ProcessState::Exited(started)
    } else {
        ProcessState::Alive(started)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_previously_bound_lifetime_proves_exit() {
        let bound = (100, 20);
        assert_eq!(
            require_current(Some(bound), ProcessState::Alive(bound)).unwrap(),
            bound
        );
        assert_eq!(
            require_current(None, ProcessState::Alive(bound)).unwrap(),
            bound
        );
        for (state, reason) in [
            (ProcessState::Absent, "absent"),
            (ProcessState::Exited(bound), "zombie"),
            (ProcessState::Alive((101, 20)), "reused_pid"),
            (ProcessState::Exited((101, 20)), "reused_pid"),
        ] {
            let fault = require_current(Some(bound), state).unwrap_err();
            assert_eq!(fault.category, "TargetExited");
            assert_eq!(fault.context["exit_reason"], reason);
        }
        for state in [ProcessState::Absent, ProcessState::Exited(bound)] {
            assert_eq!(
                require_current(None, state).unwrap_err().category,
                "TargetLost"
            );
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn failed_or_partial_lookup_and_bad_metadata_never_prove_exit() {
        for (returned, errno) in [
            (0, libc::EPERM),
            (0, libc::EACCES),
            (0, 0),
            (1, libc::ESRCH),
        ] {
            assert_eq!(
                classify_failure(returned, errno).unwrap_err().category,
                "Blocked"
            );
        }
        assert!(matches!(
            classify_failure(0, libc::ESRCH).unwrap(),
            ProcessState::Absent
        ));
        for (actual, started) in [(43, (10, 1)), (42, (0, 1)), (42, (10, 1_000_000))] {
            assert_eq!(
                classify_metadata(42, actual, libc::SZOMB, started)
                    .unwrap_err()
                    .category,
                "Blocked"
            );
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn current_process_retains_its_kernel_lifetime_without_native_authority() {
        let pid = i32::try_from(std::process::id()).unwrap();
        let first = require_current(None, probe(pid).unwrap()).unwrap();
        assert_eq!(
            require_current(Some(first), probe(pid).unwrap()).unwrap(),
            first
        );
        assert_eq!(probe(0).unwrap_err().category, "Blocked");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn normally_exited_owned_process_is_positive_absence_not_lookup_failure() {
        use std::process::{Command, Stdio};
        let mut child = Command::new("/bin/cat")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let pid = i32::try_from(child.id()).unwrap();
        let before = require_current(None, probe(pid).unwrap());
        drop(child.stdin.take());
        assert!(child.wait().unwrap().success());
        let fault = require_current(Some(before.unwrap()), probe(pid).unwrap()).unwrap_err();
        assert_eq!(fault.category, "TargetExited");
        assert_eq!(fault.context["exit_reason"], "absent");
    }
}
