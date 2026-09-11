//! The hub's internals, exposed as a library so the HTTP surface can be
//! exercised end to end from an integration test.

pub mod alerts;
pub mod config;
pub mod db;
pub mod email;
pub mod fleet;
pub mod html;
pub mod trend;
pub mod web;
