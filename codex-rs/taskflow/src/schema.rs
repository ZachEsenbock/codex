use serde::Deserialize;
use serde::Serialize;
use std::collections::BTreeMap;
use std::collections::BTreeSet;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskFile {
    pub version: String,
    pub objective: String,
    #[serde(default)]
    pub agents: Vec<AgentSpec>,
    #[serde(default)]
    pub tasks: Vec<TaskSpec>,

    #[serde(default)]
    pub concurrency: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSpec {
    pub id: String,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub profile: Option<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub sandbox: Option<String>,
    #[serde(default)]
    pub ask_for_approval: Option<String>, // never|on-request|always
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskSpec {
    pub id: String,
    #[serde(default)]
    pub title: Option<String>,
    pub agent: String,
    pub instructions: String,
    #[serde(default)]
    pub depends_on: Vec<String>,

    #[serde(default)]
    pub env: BTreeMap<String, String>,

    #[serde(default)]
    pub retries: Option<u32>,

    #[serde(default)]
    pub timeout_secs: Option<u64>,

    #[serde(default)]
    pub cwd: Option<String>,
}

impl TaskFile {
    pub fn validate(&self) -> Result<(), String> {
        if self.version != "1" {
            return Err(format!("unsupported version: {}", self.version));
        }
        let mut seen = BTreeSet::new();
        for t in &self.tasks {
            if !seen.insert(&t.id) {
                return Err(format!("duplicate task id: {}", t.id));
            }
        }
        Ok(())
    }

    pub fn agent_by_id(&self, id: &str) -> Option<&AgentSpec> {
        self.agents.iter().find(|a| a.id == id)
    }
}
