use std::time::Duration;

use oxap_adapter::{Descriptor, Error, OpenRequest};

#[derive(Clone, Debug, Default)]
pub struct Config {
    pub executable: String,
    pub args: Vec<String>,
    pub environment: Vec<String>,
    pub working_directory: String,
    pub model: String,
    pub journal_capacity: usize,
    pub frame_limit: usize,
    pub queue_capacity: usize,
    pub request_timeout: Duration,
}

#[derive(Clone, Debug)]
pub struct Adapter {
    config: Config,
}

impl Adapter {
    pub fn new(config: Config) -> Result<Self, Error> {
        Ok(Self { config })
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    pub fn describe(&self) -> Descriptor {
        Descriptor::default()
    }

    pub fn admit(&self, _request: &OpenRequest) -> Result<(), Error> {
        Ok(())
    }
}
