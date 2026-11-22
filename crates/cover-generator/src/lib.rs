pub mod background;
pub mod canvas;
pub mod color;
mod error;
pub mod styles;
pub mod text;

pub use error::CoverError;
pub use image::DynamicImage;
pub use styles::{BackgroundOption, CoverOptions, CoverStyle, generate_cover};
