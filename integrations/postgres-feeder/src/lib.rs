//! Reference PostgreSQL feeder for Knotrel.
//!
//! Reads two tables (nodes and edges) of one PostgreSQL database through
//! logical replication (`pgoutput`) and keeps a Knotrel server in
//! source-managed mode synchronized with them. The protocol, its assumptions
//! and its correctness criteria are in
//! `docs/integrations/postgresql-feeder.md`; this crate implements that
//! document and nothing more general.

pub mod config;
pub mod fault;
pub mod feeder;
pub mod knotrel;
pub mod mapping;
pub mod pgoutput;
pub mod position;
pub mod postgres;
