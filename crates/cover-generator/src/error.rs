use thiserror::Error;

#[derive(Debug, Error)]
pub enum CoverError {
    #[error("No source images provided")]
    EmptySourceImages,

    #[error("Invalid font bytes: {0}")]
    FontError(String),

    #[error("Image processing error: {0}")]
    ImageError(#[from] image::ImageError),

    #[error("Rendering error: {0}")]
    RenderError(String),
}
