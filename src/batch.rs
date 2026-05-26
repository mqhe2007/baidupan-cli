use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{IoContext, Result};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BatchTask {
    Mkdir {
        path: String,
    },
    Rm {
        path: String,
    },
    Mv {
        from: String,
        to: String,
    },
    Cp {
        from: String,
        to: String,
    },
    Upload {
        local: PathBuf,
        remote: String,
        #[serde(default)]
        encrypt: bool,
        #[serde(default)]
        force: bool,
    },
    Download {
        remote: String,
        local: PathBuf,
        #[serde(default)]
        decrypt: bool,
        #[serde(default)]
        force: bool,
    },
}

#[derive(Debug, Clone, Serialize)]
pub struct BatchTaskResult {
    pub task: BatchTask,
    pub ok: bool,
    pub output: Option<serde_json::Value>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BatchReport {
    pub total: usize,
    pub succeeded: usize,
    pub failed: usize,
    pub results: Vec<BatchTaskResult>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum BatchManifest {
    Tasks(Vec<BatchTask>),
    Wrapped { tasks: Vec<BatchTask> },
}

pub fn load_batch_tasks(path: &Path) -> Result<Vec<BatchTask>> {
    let json = fs::read_to_string(path).at(path)?;
    let manifest: BatchManifest = serde_json::from_str(&json)?;
    Ok(match manifest {
        BatchManifest::Tasks(tasks) => tasks,
        BatchManifest::Wrapped { tasks } => tasks,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_plain_task_array_manifest() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let path = temp_dir.path().join("tasks.json");
        fs::write(&path, r#"[{"type":"mkdir","path":"/apps/demo"}]"#).expect("write");

        let tasks = load_batch_tasks(&path).expect("load tasks");

        assert_eq!(
            tasks,
            vec![BatchTask::Mkdir {
                path: "/apps/demo".to_string()
            }]
        );
    }

    #[test]
    fn loads_wrapped_manifest() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let path = temp_dir.path().join("tasks.json");
        fs::write(
            &path,
            r#"{"tasks":[{"type":"download","remote":"/apps/a.txt","local":"out.txt","force":true}]}"#,
        )
        .expect("write");

        let tasks = load_batch_tasks(&path).expect("load tasks");

        assert_eq!(
            tasks,
            vec![BatchTask::Download {
                remote: "/apps/a.txt".to_string(),
                local: PathBuf::from("out.txt"),
                decrypt: false,
                force: true,
            }]
        );
    }
}
