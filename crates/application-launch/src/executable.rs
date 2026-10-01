use crate::{LaunchDisposition, LaunchError, LaunchErrorKind};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Condvar, LazyLock, Mutex};
use std::time::Duration;

pub(super) struct PreparedExecutable {
    command: Command,
    reaper: &'static Arc<Reaper>,
}

impl PreparedExecutable {
    pub(super) fn prepare(
        path: PathBuf,
        directory: Option<PathBuf>,
        arguments: &[String],
    ) -> Result<Self, LaunchError> {
        let command = command(&path, directory.as_deref(), arguments)?;
        let reaper = REAPER.as_ref().map_err(|error| *error)?;
        Ok(Self { command, reaper })
    }

    pub(super) fn submit(mut self) -> Result<LaunchDisposition, LaunchError> {
        let mut children = self
            .reaper
            .children
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        children.reserve(1);
        let child = self.command.spawn().map_err(|error| LaunchError {
            kind: LaunchErrorKind::Rejected,
            disposition: LaunchDisposition::Rejected,
            os_code: error.raw_os_error(),
        })?;
        // No fallible ownership transfer after spawn; this mutex is also the reaper's queue.
        children.push(child);
        self.reaper.wake.notify_one();
        Ok(LaunchDisposition::Accepted)
    }
}

fn command(
    path: &Path,
    directory: Option<&Path>,
    arguments: &[String],
) -> Result<Command, LaunchError> {
    let invalid = || LaunchError::before_submission(LaunchErrorKind::InvalidRequest);
    if !path.is_absolute() || !path.is_file() {
        return Err(invalid());
    }
    // std::process wraps Windows batch files in cmd.exe, violating direct argv.
    #[cfg(windows)]
    if path
        .extension()
        .and_then(std::ffi::OsStr::to_str)
        .is_some_and(|extension| {
            extension.eq_ignore_ascii_case("bat") || extension.eq_ignore_ascii_case("cmd")
        })
    {
        return Err(LaunchError::before_submission(
            LaunchErrorKind::UnsupportedPlatform,
        ));
    }
    let directory = directory.or_else(|| path.parent()).ok_or_else(invalid)?;
    if !directory.is_absolute() || !directory.is_dir() {
        return Err(invalid());
    }
    let mut command = Command::new(path);
    command
        .args(arguments)
        .current_dir(directory)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    Ok(command)
}

struct Reaper {
    children: Mutex<Vec<Child>>,
    wake: Condvar,
}

// External application lifetime is intentionally independent of each submitting operation.
// While the application is alive all children remain owned until reaped; process exit
// leaves any still-running external children to the operating system, without killing them.
static REAPER: LazyLock<Result<Arc<Reaper>, LaunchError>> = LazyLock::new(|| {
    let reaper = Arc::new(Reaper {
        children: Mutex::new(Vec::new()),
        wake: Condvar::new(),
    });
    let worker = Arc::clone(&reaper);
    std::thread::Builder::new()
        .name("application-launch-reaper".into())
        .spawn(move || reap(&worker))
        .map_err(|_| LaunchError::before_submission(LaunchErrorKind::Preparation))?;
    Ok(reaper)
});

fn reap(worker: &Reaper) {
    let mut children = worker
        .children
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    loop {
        while children.is_empty() {
            children = worker
                .wake
                .wait(children)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        let mut index = 0;
        while index < children.len() {
            let settled = match children[index].try_wait() {
                Ok(Some(_)) => true,
                Err(error) => no_longer_a_child(&error),
                Ok(None) => false,
            };
            if settled {
                children.swap_remove(index);
            } else {
                index += 1;
            }
        }
        if !children.is_empty() {
            (children, _) = worker
                .wake
                .wait_timeout(children, Duration::from_millis(250))
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }
}

fn no_longer_a_child(error: &std::io::Error) -> bool {
    #[cfg(unix)]
    return error.raw_os_error() == Some(libc::ECHILD);
    #[cfg(not(unix))]
    {
        let _ = error;
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn batch_recipients_refuse_before_implicit_shell_submission() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        for extension in ["bAt", "cMd"] {
            let path = std::env::temp_dir().join(format!(
                "mado-launch-{}-{nonce}.{extension}",
                std::process::id()
            ));
            drop(
                std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&path)
                    .unwrap(),
            );
            let result = PreparedExecutable::prepare(path.clone(), None, &[]);
            std::fs::remove_file(path).unwrap();
            let error = result.err().expect("batch files require an implicit shell");
            assert_eq!(error.kind, LaunchErrorKind::UnsupportedPlatform);
            assert_eq!(error.disposition, LaunchDisposition::NotRequested);
        }
    }

    #[cfg(unix)]
    #[test]
    fn direct_process_receives_literal_argument_boundaries() {
        let arguments = ["<%s>\\n", "", "two words", "$HOME;*", "--"].map(String::from);
        let output = command(Path::new("/usr/bin/printf"), None, &arguments)
            .unwrap()
            .stdout(Stdio::piped())
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, b"<>\n<two words>\n<$HOME;*>\n<-->\n");
    }

    #[cfg(unix)]
    #[test]
    fn direct_process_uses_explicit_or_executable_parent_directory() {
        let path = Path::new("/bin/pwd");
        let explicit = std::env::temp_dir();
        for directory in [None, Some(explicit.as_path())] {
            let output = command(path, directory, &[])
                .unwrap()
                .stdout(Stdio::piped())
                .output()
                .unwrap();
            assert!(output.status.success());
            let observed = PathBuf::from(String::from_utf8(output.stdout).unwrap().trim());
            let expected = directory
                .unwrap_or_else(|| path.parent().unwrap())
                .canonicalize()
                .unwrap();
            assert_eq!(observed.canonicalize().unwrap(), expected);
        }
    }

    #[cfg(unix)]
    #[test]
    fn accepted_child_outlives_submission_and_is_reaped_after_exit() {
        use std::os::fd::OwnedFd;
        use std::os::unix::net::UnixStream;
        use std::time::Instant;

        let (input, hold_open) = UnixStream::pair().unwrap();
        let mut prepared =
            PreparedExecutable::prepare(PathBuf::from("/bin/cat"), None, &[]).unwrap();
        prepared.command.stdin(OwnedFd::from(input));
        let reaper = prepared.reaper;
        let (send, receive) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let _ = send.send(prepared.submit());
        });
        let receipt = receive.recv_timeout(Duration::from_secs(5));
        if receipt.is_err() {
            drop(hold_open);
            worker.join().unwrap();
            panic!("submission did not return while the external process remained alive");
        }
        assert_eq!(receipt.unwrap().unwrap(), LaunchDisposition::Accepted);
        worker.join().unwrap();
        let pid = {
            let mut children = reaper
                .children
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let child = children
                .last_mut()
                .expect("the live external process remains owned");
            assert!(
                child.try_wait().unwrap().is_none(),
                "submission must not wait for external exit"
            );
            child.id()
        };
        drop(hold_open);
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let children = reaper
                .children
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if !children.iter().any(|child| child.id() == pid) {
                break;
            }
            drop(children);
            assert!(
                Instant::now() < deadline,
                "the exited external process was not reaped"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn relative_or_invalid_directories_refuse_before_submission() {
        let path = std::env::current_exe().unwrap();
        for result in [
            command(Path::new("relative-executable"), None, &[]),
            command(&path, Some(Path::new("relative-directory")), &[]),
            command(&path, Some(&path), &[]),
        ] {
            let error = result.unwrap_err();
            assert_eq!(error.kind, LaunchErrorKind::InvalidRequest);
            assert_eq!(error.disposition, LaunchDisposition::NotRequested);
        }
    }
}
