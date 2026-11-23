use thiserror::Error;

#[derive(Error, Debug)]
pub enum Error {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("image: {0}")]
    Image(String),
    #[error("develop failed: {0}")]
    Develop(String),
    #[error("lut: {0}")]
    Lut(String),
    #[error("config: {0}")]
    Config(String),
    #[error("model: {0}")]
    Model(String),
    #[error("thread pool: {0}")]
    ThreadPool(String),
}

pub type Result<T> = std::result::Result<T, Error>;