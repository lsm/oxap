use std::time::Duration;

use crossbeam_channel::Receiver;
use oxap_adapter::process::{shuttable, shutter_pair, wait_error, Child, ChildConfig};

use crate::client::Client;

const WAIT_DELAY: Duration = Duration::from_secs(1);

#[derive(Clone, Debug, Default)]
pub struct ProcessConfig {
    pub path: String,
    pub args: Vec<String>,
    pub dir: String,
    pub env: Vec<String>,
    pub frame_limit: usize,
    pub queue_capacity: usize,
}

pub struct Process {
    pub client: Client,
    child: Child,
}

impl Process {
    pub fn start(config: &ProcessConfig) -> std::io::Result<Self> {
        let (shut, shutter) = shutter_pair()?;
        let (child, stdout) = Child::spawn(&ChildConfig {
            path: config.path.clone(),
            args: config.args.clone(),
            dir: config.dir.clone(),
            env: Some(config.env.clone()),
            stderr_limit: 0,
        })?;
        let wire = child.clone();
        let client = Client::new(
            shuttable(stdout, shut),
            child.writer(),
            Box::new(move || {
                wire.release_stdin();
                drop(shutter);
                wire.kill();
            }),
            config.frame_limit,
            config.queue_capacity,
        );
        let (stopping, stopped) = crossbeam_channel::bounded::<()>(0);
        let (watcher, failed) = (client.clone(), client.done());
        std::thread::spawn(move || {
            let _ = failed.recv();
            watcher.close();
            drop(stopping);
        });
        child.watch(stopped, WAIT_DELAY);
        Ok(Self { client, child })
    }

    pub fn done(&self) -> Receiver<()> {
        self.child.exited()
    }

    pub fn wait_error(&self) -> Option<String> {
        let _ = self.child.exited().recv();
        wait_error(self.child.exit_status())
    }

    pub fn close(&self) {
        self.client.close();
        let _ = self.child.exited().recv();
    }
}
