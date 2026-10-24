use crate::{ChSpec, ScratchError};

pub struct ClickHouseServer {
    url: String,
    version: String,
}

impl ClickHouseServer {
    pub async fn start(_spec: &ChSpec) -> Result<Self, ScratchError> {
        unimplemented!("clickhouse scratch")
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    pub fn version(&self) -> &str {
        &self.version
    }

    pub async fn stop(self) -> Result<(), ScratchError> {
        unimplemented!("clickhouse scratch")
    }
}
