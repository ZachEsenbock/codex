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

    #[error("signal operation not supported on this platform: {0}")]
    SignalUnsupported(&'static str),

    #[error("failed to send signal: {0}")]
    Signal(String),

    #[error("child stdin is not available or closed")]
    StdinClosed,

    #[error("injection write failed: {0}")]
    Injection(String),
}
