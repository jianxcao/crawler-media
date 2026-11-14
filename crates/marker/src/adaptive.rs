pub mod model;
pub mod planner;
pub mod types;
pub mod verify;
pub const TEMPLATE_MODEL_VERSION: u32 = 1;

pub use model::{build_season_models, select_seed_episodes};
pub use planner::plan_episode_window;
pub use types::{
    EpisodeDescriptor, EpisodeEvidence, SamplingMode, SamplingPolicy, SegmentKind,
    SourceCostSummary, TemplateContext, TemplateModel, TemplateReference, VerificationOutcome,
    VerifiedInterval, WindowDecision,
};
pub use verify::{verify_template_window, verify_template_window_with_chapters};
