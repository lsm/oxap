use std::io::{Read, Write};
use std::process::{ChildStdin, ChildStdout, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard};
use std::time::Duration;

use crossbeam_channel::Receiver;
use regex::Regex;

const DEFAULT_STDERR_LIMIT: usize = 64 << 10;

#[derive(Clone, Debug, Default)]
pub struct ChildConfig {
    pub path: String,
    pub args: Vec<String>,
    pub dir: String,
    pub env: Option<Vec<String>>,
    pub stderr_limit: usize,
}

#[derive(Default)]
struct Stderr {
    bytes: Vec<u8>,
    truncated: bool,
}

#[derive(Clone)]
pub struct Child {
    child: Arc<Mutex<std::process::Child>>,
    stdin: Arc<Stdin>,
    stderr: Arc<Mutex<Stderr>>,
    stderr_done: Receiver<()>,
    stderr_shut: Arc<Mutex<Option<Shutter>>>,
    exited: Receiver<()>,
    exit: Arc<Mutex<Option<crossbeam_channel::Sender<()>>>>,
    status: Arc<Mutex<Option<ExitStatus>>>,
    killed: Receiver<()>,
    kill: Arc<Mutex<Option<crossbeam_channel::Sender<()>>>>,
}

#[cfg(unix)]
pub type Shutter = std::os::unix::net::UnixStream;
#[cfg(not(unix))]
pub type Shutter = ();

#[cfg(unix)]
pub fn shutter_pair() -> std::io::Result<(Shutter, Shutter)> {
    std::os::unix::net::UnixStream::pair()
}

#[cfg(not(unix))]
pub fn shutter_pair() -> std::io::Result<(Shutter, Shutter)> {
    Ok(((), ()))
}

#[cfg(unix)]
pub fn shuttable<P>(pipe: P, shut: Shutter) -> impl Read + Send + 'static
where
    P: Read + std::os::fd::AsFd + Send + 'static,
{
    struct Shuttable<P> {
        pipe: P,
        shut: Shutter,
    }
    impl<P: Read + std::os::fd::AsFd> Read for Shuttable<P> {
        fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
            use rustix::event::{PollFd, PollFlags};
            let mut descriptors = [
                PollFd::new(&self.pipe, PollFlags::IN),
                PollFd::new(&self.shut, PollFlags::IN),
            ];
            poll_uninterrupted(&mut descriptors)?;
            if !descriptors[1].revents().is_empty() {
                return Ok(0);
            }
            self.pipe.read(out)
        }
    }
    Shuttable { pipe, shut }
}

#[cfg(unix)]
fn poll_uninterrupted(descriptors: &mut [rustix::event::PollFd<'_>]) -> std::io::Result<()> {
    loop {
        match rustix::event::poll(descriptors, None) {
            Err(errno) if errno == rustix::io::Errno::INTR => {}
            outcome => return outcome.map(drop).map_err(Into::into),
        }
    }
}

#[cfg(not(unix))]
pub fn shuttable<P: Read + Send + 'static>(pipe: P, _shut: Shutter) -> impl Read + Send + 'static {
    pipe
}

struct Stdin {
    pipe: Mutex<Option<ChildStdin>>,
    released: AtomicBool,
    watch: Shutter,
    shut: Mutex<Option<Shutter>>,
}

impl Stdin {
    fn release(&self) {
        self.released.store(true, Ordering::SeqCst);
        lock(&self.shut).take();
        if let Ok(mut pipe) = self.pipe.try_lock() {
            pipe.take();
        }
    }

    fn with<T>(
        &self,
        use_pipe: impl FnOnce(&mut ChildStdin) -> std::io::Result<T>,
        absent: impl FnOnce() -> std::io::Result<T>,
    ) -> std::io::Result<T> {
        if self.released.load(Ordering::SeqCst) {
            lock(&self.pipe).take();
            return absent();
        }
        let mut pipe = lock(&self.pipe);
        let outcome = match pipe.as_mut() {
            Some(stdin) => use_pipe(stdin),
            None => absent(),
        };
        if self.released.load(Ordering::SeqCst) {
            pipe.take();
        }
        outcome
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poison| poison.into_inner())
}

static SECRET_LINE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(authorization|x-api-key|api[_-]?key|auth[_-]?token|password|secret|token)(\s*[:=]\s*)(?:Bearer\s+)?([^\s,;]+)").expect("pattern")
});
static SECRET_QUOTED: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)((?:authorization|x-api-key|api[_-]?key|auth[_-]?token|password|secret|token)"?\s*[:=]\s*)"[^"]*""#).expect("pattern")
});

pub fn redact(value: &str) -> String {
    let quoted = SECRET_QUOTED.replace_all(value, "${1}\"[REDACTED]\"");
    SECRET_LINE
        .replace_all(&quoted, "${1}${2}[REDACTED]")
        .trim()
        .to_owned()
}

#[cfg(unix)]
fn signal_name(signal: i32) -> Option<&'static str> {
    let shared = match signal {
        1 => "hangup",
        2 => "interrupt",
        3 => "quit",
        4 => "illegal instruction",
        8 => "floating point exception",
        9 => "killed",
        11 => "segmentation fault",
        13 => "broken pipe",
        14 => "alarm clock",
        15 => "terminated",
        _ => "",
    };
    if !shared.is_empty() {
        return Some(shared);
    }
    #[cfg(target_os = "linux")]
    let named = match signal {
        5 => "trace/breakpoint trap",
        6 => "aborted",
        7 => "bus error",
        10 => "user defined signal 1",
        12 => "user defined signal 2",
        16 => "stack fault",
        17 => "child exited",
        18 => "continued",
        19 => "stopped (signal)",
        20 => "stopped",
        21 => "stopped (tty input)",
        22 => "stopped (tty output)",
        23 => "urgent I/O condition",
        24 => "CPU time limit exceeded",
        25 => "file size limit exceeded",
        26 => "virtual timer expired",
        27 => "profiling timer expired",
        28 => "window changed",
        29 => "I/O possible",
        30 => "power failure",
        31 => "bad system call",
        _ => return None,
    };
    #[cfg(not(target_os = "linux"))]
    let named = match signal {
        5 => "trace/BPT trap",
        6 => "abort trap",
        7 => "EMT trap",
        10 => "bus error",
        12 => "bad system call",
        16 => "urgent I/O condition",
        17 => "suspended (signal)",
        18 => "suspended",
        19 => "continued",
        20 => "child exited",
        21 => "stopped (tty input)",
        22 => "stopped (tty output)",
        23 => "I/O possible",
        24 => "cputime limit exceeded",
        25 => "filesize limit exceeded",
        26 => "virtual timer expired",
        27 => "profiling timer expired",
        28 => "window size changes",
        29 => "information request",
        30 => "user defined signal 1",
        31 => "user defined signal 2",
        _ => return None,
    };
    Some(named)
}

pub fn wait_error(status: Option<ExitStatus>) -> Option<String> {
    let status = status?;
    if status.success() {
        return None;
    }
    if let Some(code) = status.code() {
        return Some(format!("exit status {code}"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            let name = signal_name(signal).map_or_else(|| format!("signal {signal}"), String::from);
            let core = if status.core_dumped() {
                " (core dumped)"
            } else {
                ""
            };
            return Some(format!("signal: {name}{core}"));
        }
    }
    Some("process exited".into())
}

impl Child {
    pub fn spawn(config: &ChildConfig) -> std::io::Result<(Self, ChildStdout)> {
        let mut command = Command::new(&config.path);
        command.args(&config.args);
        if !config.dir.is_empty() {
            command.current_dir(&config.dir);
        }
        if let Some(env) = &config.env {
            command.env_clear();
            for entry in env {
                if let Some((name, value)) = entry.split_once('=') {
                    command.env(name, value);
                }
            }
        }
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let (shut, shutter) = shutter_pair()?;
        let (stdin_watch, stdin_shut) = shutter_pair()?;
        let mut child = command.spawn()?;
        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = child.stdout.take().expect("piped stdout");
        let mut pipe = shuttable(child.stderr.take().expect("piped stderr"), shut);
        let limit = if config.stderr_limit == 0 {
            DEFAULT_STDERR_LIMIT
        } else {
            config.stderr_limit
        };
        let stderr = Arc::new(Mutex::new(Stderr::default()));
        let (stderr_sender, stderr_done) = crossbeam_channel::bounded::<()>(0);
        let captured = stderr.clone();
        std::thread::spawn(move || {
            let mut chunk = [0u8; 4096];
            while let Ok(read) = pipe.read(&mut chunk) {
                if read == 0 {
                    break;
                }
                let mut held = lock(&captured);
                let remaining = limit.saturating_sub(held.bytes.len());
                if read > remaining {
                    held.truncated = true;
                }
                let take = read.min(remaining);
                held.bytes.extend_from_slice(&chunk[..take]);
            }
            drop(stderr_sender);
        });
        let (exit, exited) = crossbeam_channel::bounded::<()>(0);
        let (kill, killed) = crossbeam_channel::bounded::<()>(0);
        Ok((
            Self {
                child: Arc::new(Mutex::new(child)),
                stdin: Arc::new(Stdin {
                    pipe: Mutex::new(Some(stdin)),
                    released: AtomicBool::new(false),
                    watch: stdin_watch,
                    shut: Mutex::new(Some(stdin_shut)),
                }),
                stderr,
                stderr_done,
                stderr_shut: Arc::new(Mutex::new(Some(shutter))),
                exited,
                exit: Arc::new(Mutex::new(Some(exit))),
                status: Arc::new(Mutex::new(None)),
                killed,
                kill: Arc::new(Mutex::new(Some(kill))),
            },
            stdout,
        ))
    }

    pub fn writer(&self) -> impl Write + Send + 'static {
        SharedStdin(self.stdin.clone())
    }

    pub fn release_stdin(&self) {
        self.stdin.release();
    }

    pub fn watch(&self, reader_done: Receiver<()>, drain: Duration) {
        let child = self.clone();
        std::thread::spawn(move || {
            crossbeam_channel::select! {
                recv(reader_done) -> _ => {}
                recv(child.killed) -> _ => {}
            }
            let killed = crossbeam_channel::select! {
                recv(child.stderr_done) -> _ => false,
                recv(child.killed) -> _ => true,
                default(drain) => false,
            };
            if !killed {
                child.close_stderr();
            }
            let waited = loop {
                if let Ok(Some(exit)) = lock(&child.child).try_wait() {
                    break Some(exit);
                }
                std::thread::sleep(Duration::from_millis(10));
            };
            *lock(&child.status) = waited;
            lock(&child.exit).take();
            child.release_stdin();
        });
    }

    pub fn exited(&self) -> Receiver<()> {
        self.exited.clone()
    }

    pub fn exit_status(&self) -> Option<ExitStatus> {
        *lock(&self.status)
    }

    pub fn stderr(&self) -> String {
        let held = lock(&self.stderr);
        let mut text = String::from_utf8_lossy(&held.bytes).into_owned();
        if held.truncated {
            text.push_str(" [truncated]");
        }
        redact(&text)
    }

    pub fn drain_stderr(&self, within: Duration) {
        let _ = self.stderr_done.recv_timeout(within);
        self.close_stderr();
    }

    pub fn release_stderr(&self) {
        lock(&self.stderr_shut).take();
    }

    #[cfg(unix)]
    fn close_stderr(&self) {
        self.release_stderr();
        let _ = self.stderr_done.recv();
    }

    #[cfg(not(unix))]
    fn close_stderr(&self) {}

    pub fn kill(&self) {
        let _ = lock(&self.child).kill();
        lock(&self.kill).take();
    }

    #[cfg(unix)]
    pub fn interrupt(&self) {
        let mut child = lock(&self.child);
        if !matches!(child.try_wait(), Ok(None)) {
            return;
        }
        let id = child.id().to_string();
        let sent = ["/bin/kill", "/usr/bin/kill"].iter().any(|kill| {
            Command::new(kill)
                .args(["-INT", &id])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|status| status.success())
        });
        if !sent {
            let _ = child.kill();
        }
    }

    #[cfg(not(unix))]
    pub fn interrupt(&self) {
        self.kill();
    }

    pub fn await_exit(&self, within: Duration) -> bool {
        matches!(
            self.exited.recv_timeout(within),
            Err(crossbeam_channel::RecvTimeoutError::Disconnected)
        )
    }
}

struct SharedStdin(Arc<Stdin>);

impl Write for SharedStdin {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        let watch = &self.0.watch;
        self.0.with(
            |stdin| write_unless_shut(stdin, watch, data),
            || Err(std::io::ErrorKind::BrokenPipe.into()),
        )
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.0.with(|stdin| stdin.flush(), || Ok(()))
    }
}

#[cfg(unix)]
fn write_unless_shut(
    stdin: &mut ChildStdin,
    watch: &Shutter,
    data: &[u8],
) -> std::io::Result<usize> {
    use rustix::event::{PollFd, PollFlags};
    let mut descriptors = [
        PollFd::new(stdin, PollFlags::OUT),
        PollFd::new(watch, PollFlags::IN),
    ];
    poll_uninterrupted(&mut descriptors)?;
    if !descriptors[1].revents().is_empty() {
        return Err(std::io::ErrorKind::BrokenPipe.into());
    }
    stdin.write(&data[..data.len().min(PIPE_ATOMIC)])
}

#[cfg(unix)]
const PIPE_ATOMIC: usize = 512;

#[cfg(not(unix))]
fn write_unless_shut(
    stdin: &mut ChildStdin,
    _watch: &Shutter,
    data: &[u8],
) -> std::io::Result<usize> {
    stdin.write(data)
}
