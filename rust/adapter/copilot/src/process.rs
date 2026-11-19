use crossbeam_channel::Receiver;

use crate::client::Client;

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
}

impl Process {
    pub fn start(_config: &ProcessConfig) -> std::io::Result<Self> {
        Err(std::io::Error::other(
            "copilot rpc: process start is not ported",
        ))
    }

    pub fn done(&self) -> Receiver<()> {
        self.client.done()
    }

    pub fn wait_error(&self) -> Option<String> {
        None
    }

    pub fn close(&self) {
        self.client.close();
    }
}
