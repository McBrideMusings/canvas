//! The guidance block printed to an agent's context on session start, and by
//! `canvas guidance` on demand. Compiled into the binary so it ships with no
//! runtime file dependency.

pub const TEXT: &str = include_str!("../../plugin/guidance.md");
