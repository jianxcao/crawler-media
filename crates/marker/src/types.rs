use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MarkerType {
    IntroStart,
    IntroEnd,
    CreditsStart,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Chapter {
    pub start_ms: i64,
    pub end_ms: i64,
    pub title: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChapterMarker {
    pub start_ms: i64,
    pub end_ms: i64,
    pub title: Option<String>,
    pub marker_type: Option<MarkerType>,
    #[serde(default)]
    pub synthetic: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CommonSegment {
    pub start1_sec: f32,
    pub end1_sec: f32,
    pub start2_sec: f32,
    pub end2_sec: f32,
    pub duration_sec: f32,
    pub score: f64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DetectedIntro {
    pub episode: u32,
    pub intro_start_ms: i64,
    pub intro_end_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DetectedOutro {
    pub episode: u32,
    pub outro_start_ms: i64,
    pub outro_end_ms: i64,
}
