//! releasectl のコア。desired-state の型・検証と Action の型だけを持ち、
//! ネットワークにも鍵にも触れない（Cloud 側の wasm からも使えるようにするため）。

pub mod action;
pub mod config;

pub use action::{Action, Report, Status};
pub use config::{Config, ConfigError, Product};
