//! releasectl のコア。desired-state の型・検証と Action の型だけを持ち、
//! ネットワークにも鍵にも触れない（Cloud 側の wasm からも使えるようにするため）。

pub mod action;
pub mod config;
pub mod iap_init;
pub mod markdown;
pub mod meta;
pub mod meta_init;
pub mod yaml_out;

pub use action::{
    Action, AppStoreVersion, JSON_VERSION, Money, Report, RunReport, Status, Summary, exit_code,
};
pub use config::{Config, ConfigError, Product};
