use std::{fs::File, path::PathBuf, sync::Arc};

use tracing_subscriber::EnvFilter;

/// Logger configuration.
#[derive(Debug, Clone, Default)]
pub struct LoggerConfig {
    pub env_filter: String,
    pub log_file_path: Option<PathBuf>,
    pub log_json: bool,
    pub log_color: bool,
}

impl LoggerConfig {
    /// Initialize tracing subscriber based on the configuration.
    pub fn init_tracing(self) -> eyre::Result<()> {
        let env_filter = EnvFilter::try_new(&self.env_filter)?;
        let builder = tracing_subscriber::fmt().with_env_filter(env_filter);
    
        let result = if let Some(file) = &self.log_file_path {
            let file = Arc::new(File::create(file)?);
            if self.log_json {
                builder.json().with_writer(file).try_init()
            } else {
                builder
                    .with_ansi(self.log_color)
                    .with_writer(file)
                    .try_init()
            }
        } else if self.log_json {
            builder.json().try_init()
        } else {
            builder.with_ansi(self.log_color).try_init()
        };
        
        result.map_err(|err| eyre::format_err!("{err}"))
    }
}
