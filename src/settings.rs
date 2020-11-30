use config::{Config, ConfigError, Environment, File};
use serde::Deserialize;
use std::env;
use std::net::SocketAddr;

#[derive(Clone, Debug, Deserialize)]
pub struct Settings {
    pub cache_server: HttpServer,
    pub metrics_server: HttpServer,
    pub logger_config_file: String,
    pub cache: Cache,
}

#[derive(Clone, Debug, Deserialize)]
pub struct HttpServer {
    pub workers: Option<usize>,
    pub backlog: Option<i32>,
    pub max_connections: Option<usize>,
    pub max_connection_rate: Option<usize>,
    pub client_timeout: Option<u64>,
    pub client_shutdown: Option<u64>,
    pub shutdown_timeout: Option<u64>,
    pub listen_addresses: Vec<SocketAddr>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Cache {
    pub key_live_duration: u64,
}

impl Settings {
    pub fn new() -> Result<Self, ConfigError> {
        let mut s = Config::new();

        // Start off by merging in the "default" configuration file
        s.merge(File::with_name("config/default"))?;

        // Add in the current environment file
        // Default to 'development' env
        // Note that this file is _optional_
        let env = env::var("RUN_MODE").unwrap_or_else(|_| "development".into());
        s.merge(File::with_name(&format!("config/{}", env)).required(false))?;

        // Add in a local configuration file
        // This file shouldn't be checked in to git
        s.merge(File::with_name("config/local").required(false))?;

        // Add in settings from the environment (with a prefix of APP)
        // Eg.. `APP_DEBUG=1 ./target/app` would set the `debug` key
        s.merge(Environment::with_prefix("app"))?;

        // You can deserialize (and thus freeze) the entire configuration as
        s.try_into()
    }
}

#[macro_export]
macro_rules! config_items {
    ($target:ident = $setting:ident; $($config_item:ident),*) => {
        $(
            if let Some($config_item) = $setting.$config_item {
                $target = $target.$config_item($config_item);
            }
        )*
    };
}
