use crate::{PgSpec, ScratchError};

pub struct PostgresServer {
    url: String,
    version: String,
}

impl PostgresServer {
    pub async fn start(_spec: &PgSpec) -> Result<Self, ScratchError> {
        unimplemented!("postgres scratch")
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    pub fn version(&self) -> &str {
        &self.version
    }

    pub async fn stop(self) -> Result<(), ScratchError> {
        unimplemented!("postgres scratch")
    }
}
