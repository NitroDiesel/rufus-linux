use std::fmt;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct JobId(pub u128);

impl JobId {
    pub const fn new(value: u128) -> Self {
        Self(value)
    }
}

impl fmt::Display for JobId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:032x}", self.0)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum ProgressStage {
    Preparing,
    Authorizing,
    Unmounting,
    TestingMedia,
    Wiping,
    Partitioning,
    Formatting,
    WritingImage,
    ExtractingFiles,
    InstallingBootloader,
    ApplyingCustomization,
    Syncing,
    Verifying,
    Finalizing,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum ProgressUnit {
    Bytes,
    Blocks,
    Files,
    Steps,
    Indeterminate,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Cancellability {
    Immediate,
    AtStageBoundary,
    NotCancellable,
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Progress {
    pub job_id: JobId,
    pub stage: ProgressStage,
    pub unit: ProgressUnit,
    pub completed: u64,
    pub total: Option<u64>,
    pub bytes_per_second: Option<u64>,
    pub eta_seconds: Option<u64>,
    pub detail: Option<String>,
    pub cancellability: Cancellability,
}

impl Progress {
    pub fn validate(&self) -> Result<(), &'static str> {
        if let Some(total) = self.total {
            if self.completed > total {
                return Err("completed progress exceeds total");
            }
        }
        if self.unit == ProgressUnit::Indeterminate && self.total.is_some() {
            return Err("indeterminate progress cannot have a total");
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum CancellationState {
    Running = 0,
    Requested = 1,
}

/// Cheap cooperative-cancellation primitive shared by workers.
#[derive(Clone, Debug, Default)]
pub struct CancellationToken {
    state: Arc<AtomicU8>,
}

impl CancellationToken {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn request(&self) {
        self.state
            .store(CancellationState::Requested as u8, Ordering::Release);
    }

    pub fn state(&self) -> CancellationState {
        match self.state.load(Ordering::Acquire) {
            value if value == CancellationState::Requested as u8 => CancellationState::Requested,
            _ => CancellationState::Running,
        }
    }

    pub fn is_requested(&self) -> bool {
        self.state() == CancellationState::Requested
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cloned_tokens_share_cancellation() {
        let token = CancellationToken::new();
        let worker = token.clone();
        token.request();
        assert!(worker.is_requested());
    }

    #[test]
    fn progress_rejects_impossible_counts() {
        let progress = Progress {
            job_id: JobId::new(1),
            stage: ProgressStage::WritingImage,
            unit: ProgressUnit::Bytes,
            completed: 11,
            total: Some(10),
            bytes_per_second: None,
            eta_seconds: None,
            detail: None,
            cancellability: Cancellability::Immediate,
        };
        assert_eq!(progress.validate(), Err("completed progress exceeds total"));
    }
}
