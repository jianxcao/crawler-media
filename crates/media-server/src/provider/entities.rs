//! Bounded protocol entity and aggregate response facts.
#[derive(Clone, Copy, Debug)]
pub enum EntityKind {
    Genre,
    Studio,
}
#[derive(Clone, Debug)]
pub struct Entity {
    pub name: String,
    pub wire_id: String,
    pub child_count: u64,
}
#[derive(Clone, Debug)]
pub struct EntityPage {
    pub items: Vec<Entity>,
    pub total: u64,
}
#[derive(Clone, Debug)]
pub struct ItemCounts {
    pub movies: u64,
    pub series: u64,
    pub episodes: u64,
    pub items: u64,
}
#[derive(Clone, Debug)]
pub struct Season {
    pub season: u32,
    pub total: u64,
    pub played: u64,
}
