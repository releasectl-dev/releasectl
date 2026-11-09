//! releasectl のコア。desired-state の型・検証と Action の型だけを持ち、
//! ネットワークにも鍵にも触れない（Cloud 側の wasm からも使えるようにするため）。

pub mod action;
pub mod config;
pub mod markdown;

pub use action::{Action, JSON_VERSION, Report, RunReport, Status, Summary, exit_code};
pub use config::{Config, ConfigError, Product};
