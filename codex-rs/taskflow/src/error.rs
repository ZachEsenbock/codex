use thiserror::Error;

#[derive(Error, Debug)]
pub enum TaskError {
    #[error("invalid task file: {0}")]
    InvalidTaskFile(String),

    #[error("yaml parse error: {0}")]
    Yaml(#[from] serde_yaml::Error),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("dag contains a cycle")]
    CyclicDag,

    #[error("unknown agent id: {0}")]
    UnknownAgent(String),

    #[error("duplicate task id: {0}")]
    DuplicateTask(String),

    #[error("invalid depends_on (missing task id: {0})")]
    MissingDependency(String),

    #[error("spawn failed: {0}")]
    Spawn(String),

    #[error("sub-agent failed with code {code}: {message}")]
    SubAgentFailed { code: i32, message: String },

    #[error("timeout after {0} seconds")]
    Timeout(u64),
}
