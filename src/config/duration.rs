use duration_string::DurationString;
use serde::Deserialize;
use tokio::time::Duration;

pub mod duration_serde {
    use super::*;
    use serde::Deserializer;

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Duration, D::Error>
    where
        D: Deserializer<'de>,
    {
        Ok(DurationString::deserialize(deserializer)?.into())
    }
}
