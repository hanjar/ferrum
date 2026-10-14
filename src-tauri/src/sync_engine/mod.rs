//! Sync engine for stored Sync Profiles (Automation design §5).
//!
//! Deliberately minimal in this package: only [`roots`] (root canonicalization, identity and
//! readiness probes, §5.8) lives here so far. The plan/settle/filters port and the execution
//! pipeline arrive in their own work packages as separate modules of this tree.

pub mod roots;
