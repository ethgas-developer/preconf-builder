use std::path::PathBuf;

use serde::{Deserialize, Deserializer};
use tracing_appender::{non_blocking::WorkerGuard, rolling::Rotation};
use tracing_subscriber::EnvFilter;

/// Logger configuration.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct LoggerConfig {
    pub env_filter: String,
    #[serde(default)]
    pub logging_config: LoggingConfig,
    pub log_json: bool,
    pub log_color: bool,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq, Default)]
pub enum LoggingConfig {
    #[default]
    Console,
    File {
        dir_path: PathBuf,
        file_name: String,
        #[serde(deserialize_with = "deserialize_rotation")]
        rotation: Rotation,
    },
}

fn deserialize_rotation<'de, D>(deserializer: D) -> Result<Rotation, D::Error>
where
    D: Deserializer<'de>,
{
    let s = String::deserialize(deserializer)?;
    Ok(match s.as_str() {
        "minutely" => Rotation::MINUTELY,
        "hourly" => Rotation::HOURLY,
        "daily" => Rotation::DAILY,
        "never" => Rotation::NEVER,
        _ => panic!("Unknown value, use either of | daily | minutely | hourly | never |"),
    })
}

impl LoggerConfig {
    /// Initialize tracing subscriber based on the configuration.
    pub fn init_tracing(self) -> eyre::Result<Option<WorkerGuard>> {
        let env_filter = EnvFilter::try_new(&self.env_filter)?;
        let builder = tracing_subscriber::fmt().with_env_filter(env_filter);

        if let LoggingConfig::File {
            dir_path,
            file_name,
            rotation,
        } = self.logging_config
        {
            let file_appender = tracing_appender::rolling::Builder::new()
                .filename_prefix(file_name)
                .max_log_files(30)
                .rotation(rotation)
                .build(dir_path)
                .expect("failed to create file log appender");
            let (writer, guard) = tracing_appender::non_blocking(file_appender);

            let result = if self.log_json {
                builder.json().with_writer(writer).try_init()
            } else {
                builder
                    .with_ansi(self.log_color)
                    .with_writer(writer)
                    .try_init()
            };
            result.map_err(|err| eyre::format_err!("{err}"))?;
            Ok(Some(guard))
        } else {
            let result = if self.log_json {
                builder.json().try_init()
            } else {
                builder.with_ansi(self.log_color).try_init()
            };
            result.map_err(|err| eyre::format_err!("{err}"))?;
            Ok(None)
        }
    }
}