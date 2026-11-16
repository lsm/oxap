use std::fmt;
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use crossbeam_channel::{after, never, select, Receiver};
use oxap_adapter::process::{wait_error, Child, ChildConfig};

use super::{Client, ClientError, ClientOptions};

const DEFAULT_EXIT_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Debug, Default)]
pub struct ProcessConfig {
    pub path: String,
    pub args: Vec<String>,
    pub dir: String,
    pub env: Option<Vec<String>>,
    pub frame_limit: usize,
    pub queue_capacity: usize,
    pub write_queue_capacity: usize,
    pub stderr_limit: usize,
    pub exit_timeout: Option<Duration>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProcessError {
    MissingPath,
    Spawn(String),
    NotExited,
    DeadlineExceeded,
}

impl fmt::Display for ProcessError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingPath => f.write_str("claude rpc: executable path is required"),
            Self::Spawn(detail) => f.write_str(detail),
            Self::NotExited => f.write_str("claude rpc: process did not exit after stdin EOF"),
            Self::DeadlineExceeded => f.write_str("context deadline exceeded"),
        }
    }
}

impl std::error::Error for ProcessError {}

pub struct Process {
    pub client: Client,
    child: Child,
    grace: Duration,
    closed: Mutex<Option<Result<(), ProcessError>>>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poison| poison.into_inner())
}

impl Process {
    pub fn start(config: &ProcessConfig, deadline: Option<Instant>) -> Result<Self, ProcessError> {
        if config.path.is_empty() {
            return Err(ProcessError::MissingPath);
        }
        if deadline.is_some_and(|at| Instant::now() >= at) {
            return Err(ProcessError::DeadlineExceeded);
        }
        let (child, stdout) = Child::spawn(&ChildConfig {
            path: config.path.clone(),
            args: config.args.clone(),
            dir: config.dir.clone(),
            env: config.env.clone(),
            stderr_limit: config.stderr_limit,
        })
        .map_err(|failure| ProcessError::Spawn(failure.to_string()))?;
        let releaser = child.clone();
        let client = Client::new(
            stdout,
            child.writer(),
            ClientOptions {
                frame_limit: config.frame_limit,
                queue_capacity: config.queue_capacity,
                write_queue_capacity: config.write_queue_capacity,
                closer: Some(Box::new(move || releaser.release_stdin())),
                ..ClientOptions::default()
            },
        );
        let grace = config
            .exit_timeout
            .filter(|timeout| !timeout.is_zero())
            .unwrap_or(DEFAULT_EXIT_TIMEOUT);
        child.watch(client.read_done(), grace);
        Ok(Self {
            client,
            child,
            grace,
            closed: Mutex::new(None),
        })
    }

    pub fn stderr(&self) -> String {
        self.child.stderr()
    }

    pub fn done(&self) -> Receiver<()> {
        self.child.exited()
    }

    pub fn wait_error(&self) -> Option<String> {
        wait_error(self.child.exit_status())
    }

    pub fn close(&self, deadline: Option<Instant>) -> Result<(), ProcessError> {
        let mut closed = lock(&self.closed);
        if let Some(outcome) = closed.as_ref() {
            return outcome.clone();
        }
        self.child.release_stdin();
        let expiry = deadline.map_or_else(never, |at| {
            after(at.saturating_duration_since(Instant::now()))
        });
        let exited = self.child.exited();
        let grace = after(self.grace);
        let outcome = select! {
            recv(exited) -> _ => Ok(()),
            recv(grace) -> _ => {
                self.child.interrupt();
                if !self.child.await_exit(self.grace) {
                    self.kill_and_release();
                }
                Err(ProcessError::NotExited)
            }
            recv(expiry) -> _ => {
                self.kill_and_release();
                Err(ProcessError::DeadlineExceeded)
            }
        };
        self.client.close();
        *closed = Some(outcome.clone());
        outcome
    }

    fn kill_and_release(&self) {
        self.child.kill();
        self.client
            .close_with(ClientError::Transport("claude rpc: process exited".into()));
        let _ = self.child.exited().recv();
    }
}
