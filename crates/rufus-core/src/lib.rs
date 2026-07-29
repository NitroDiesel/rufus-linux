//! Platform-neutral contracts for planning safe boot-media operations.

pub mod capability;
pub mod device;
pub mod plan;
pub mod progress;
pub mod safety;

pub use capability::{Capability, CapabilityReport, CapabilityState, MissingRequirement};
pub use device::{
    BlockDevice, DeviceClass, DeviceFingerprint, DeviceNumber, DeviceRisk, Transport,
};
pub use plan::{
    BootMode, FileSystem, ImageSource, ImageSourceKind, OperationPlan, PartitionPlan,
    PartitionScheme, PlanError, PlanStep, VerificationLevel, WriteMode,
};
pub use progress::{
    Cancellability, CancellationState, CancellationToken, JobId, Progress, ProgressStage,
    ProgressUnit,
};
pub use safety::{SafetyError, SafetyPolicy, SafetySnapshot};
