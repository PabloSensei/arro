//! The tree of a process on Unix.
//!
//! A job here is the process group of its child, and the group is signalled as a whole.
//! There is no list of jobs a program could be born outside of, so there is nothing to
//! adopt and no rounds to run.

use std::sync::Arc;

use crate::job::Job;

/// The tree of one process. Nothing to do on Unix.
pub struct ProcessTree;

impl ProcessTree {
    /// Keeps the root, which the group of the job already covers.
    pub fn new(_root: u32) -> Self {
        Self
    }

    /// Nothing is outside the group of the job.
    pub fn adopt(&self, _job: &Job) -> usize {
        0
    }

    /// The job signals the whole group.
    pub fn kill(&self, _job: &Arc<Job>) {}

    /// The job signals the whole group.
    pub fn kill_escaped(&self, _job: &Arc<Job>) {}

    /// The job signals the whole group.
    pub fn kill_blocking(&self, _job: &Job) {}
}
