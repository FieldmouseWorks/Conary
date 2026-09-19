// apps/conary-test/src/explorer/mod.rs

//! Bounded, local fixture exploration. Package semantics remain Conary-owned.
pub mod checker;
pub mod cli;
pub mod conary;
pub mod contract;
pub mod controller;
pub mod evidence;
pub mod fixtures;
pub mod jev;
pub mod reducer;
pub mod sandbox;
pub mod selector;

#[cfg(test)]
mod tests;
