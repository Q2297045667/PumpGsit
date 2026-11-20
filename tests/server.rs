//! Real-server regression tests. Run explicitly against locally supplied binaries.
#![cfg(not(target_family = "wasm"))]

#[path = "support/disconnect.rs"]
mod disconnect;
#[path = "support/mysql.rs"]
mod mysql;
#[path = "support/smoke.rs"]
mod smoke;
#[path = "support/sqlite.rs"]
mod sqlite;
mod support;
