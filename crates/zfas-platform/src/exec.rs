//! Running platform commands (`dumpsys`) with a timeout and an output cap.
//!
//! Commands are always an explicit argv; nothing is ever passed through a shell, and no
//! argument comes from configuration without validation by the caller.

use std::io::{self, Read};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

pub const MAX_OUTPUT: usize = 4 * 1024 * 1024;

#[derive(Debug)]
pub enum ExecError {
    Spawn(io::Error),
    Timeout,
    Failed { status: Option<i32>, stderr: String },
}

impl std::fmt::Display for ExecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ExecError::Spawn(e) => write!(f, "spawn failed: {e}"),
            ExecError::Timeout => write!(f, "timed out"),
            ExecError::Failed { status, stderr } => write!(f, "exit status {status:?}: {stderr}"),
        }
    }
}

impl std::error::Error for ExecError {}

/// Abstraction so tests can supply canned command output.
pub trait Runner {
    fn run(&self, argv: &[&str], timeout: Duration) -> Result<String, ExecError>;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SystemRunner;

impl Runner for SystemRunner {
    fn run(&self, argv: &[&str], timeout: Duration) -> Result<String, ExecError> {
        let (prog, args) = argv.split_first().ok_or_else(|| {
            ExecError::Spawn(io::Error::new(io::ErrorKind::InvalidInput, "empty argv"))
        })?;
        let mut child = Command::new(prog)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(ExecError::Spawn)?;
        let mut out = child.stdout.take().expect("piped");
        let mut err = child.stderr.take().expect("piped");
        let reader = thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = (&mut out).take(MAX_OUTPUT as u64).read_to_end(&mut buf);
            // Drain the rest so the child never blocks on a full pipe.
            let _ = io::copy(&mut out, &mut io::sink());
            buf
        });
        let err_reader = thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = (&mut err).take(64 * 1024).read_to_end(&mut buf);
            let _ = io::copy(&mut err, &mut io::sink());
            buf
        });
        let deadline = Instant::now() + timeout;
        let status = loop {
            match child.try_wait().map_err(ExecError::Spawn)? {
                Some(s) => break s,
                None if Instant::now() >= deadline => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(ExecError::Timeout);
                }
                None => thread::sleep(Duration::from_millis(5)),
            }
        };
        let stdout = reader.join().unwrap_or_default();
        let stderr = err_reader.join().unwrap_or_default();
        if !status.success() {
            return Err(ExecError::Failed {
                status: status.code(),
                stderr: String::from_utf8_lossy(&stderr).chars().take(500).collect(),
            });
        }
        Ok(String::from_utf8_lossy(&stdout).into_owned())
    }
}

/// Canned outputs keyed by the joined argv; for tests and replay.
#[derive(Debug, Default)]
pub struct FakeRunner {
    pub outputs: std::collections::HashMap<String, Result<String, String>>,
    pub calls: std::cell::RefCell<Vec<String>>,
}

impl FakeRunner {
    pub fn with(mut self, argv: &str, out: &str) -> Self {
        self.outputs.insert(argv.to_string(), Ok(out.to_string()));
        self
    }
}

impl Runner for FakeRunner {
    fn run(&self, argv: &[&str], _timeout: Duration) -> Result<String, ExecError> {
        let key = argv.join(" ");
        self.calls.borrow_mut().push(key.clone());
        match self.outputs.get(&key) {
            Some(Ok(s)) => Ok(s.clone()),
            Some(Err(e)) => Err(ExecError::Failed {
                status: Some(1),
                stderr: e.clone(),
            }),
            None => Err(ExecError::Spawn(io::Error::new(
                io::ErrorKind::NotFound,
                key,
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runs_and_times_out() {
        let r = SystemRunner;
        assert_eq!(
            r.run(&["echo", "hi"], Duration::from_secs(5))
                .unwrap()
                .trim(),
            "hi"
        );
        assert!(matches!(
            r.run(&["sleep", "5"], Duration::from_millis(100)),
            Err(ExecError::Timeout)
        ));
        assert!(matches!(
            r.run(&["false"], Duration::from_secs(5)),
            Err(ExecError::Failed { .. })
        ));
        assert!(matches!(
            r.run(&["/nonexistent/zfas"], Duration::from_secs(1)),
            Err(ExecError::Spawn(_))
        ));
    }
}
