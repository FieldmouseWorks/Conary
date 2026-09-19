// apps/conary-test/src/explorer/mod.rs

//! Bounded, local fixture exploration. Package semantics remain Conary-owned.
pub mod checker;
pub mod cli;
pub mod conary;
pub mod context;
pub mod contract;
pub mod controller;
pub mod evidence;
pub mod fixtures;
pub mod jev;
pub mod reducer;
pub mod sandbox;
mod selected_state;
pub mod selector;

#[cfg(test)]
mod tests;
