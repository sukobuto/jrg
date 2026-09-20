//! Search lexical candidates and rank them by a focused semantic predicate.

pub mod cli;
pub mod config;
pub mod jev;
pub mod search;

use anyhow::{Result, bail};
use std::sync::atomic::{AtomicBool, Ordering};

/// Stop pending work after Ctrl-C without exposing partial rankings.
pub fn check_cancelled(cancelled: &AtomicBool) -> Result<()> {
    if cancelled.load(Ordering::Relaxed) {
        bail!("interrupted");
    }
    Ok(())
}
