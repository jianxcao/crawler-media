pub mod model;
pub mod planner;
pub mod types;
pub mod verify;

pub use model::{build_season_models, select_seed_episodes};
pub use planner::plan_episode_window;
pub use types::{
    EpisodeDescriptor, EpisodeEvidence, SamplingMode, SamplingPolicy, SegmentKind,
    SourceCostSummary, TemplateContext, TemplateModel, TemplateReference, VerificationOutcome,
    VerifiedInterval, WindowDecision,
};
pub use verify::verify_template_window;
