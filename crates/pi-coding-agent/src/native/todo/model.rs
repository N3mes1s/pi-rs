//! Data model for the `todo` tool. Pure: no I/O, no async.
//!
//! The single invariant is "at most one [`TaskState::InProgress`] task at
//! any time" — [`Todo::normalise`] enforces it after every mutation by
//! demoting later in-progress tasks to [`TaskState::Pending`].

use serde::{Deserialize, Serialize};

/// Lifecycle states a task can be in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    Pending,
    InProgress,
    Completed,
    Abandoned,
}

impl Default for TaskState {
    fn default() -> Self {
        TaskState::Pending
    }
}

impl TaskState {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_lowercase().as_str() {
            "pending" => Some(TaskState::Pending),
            "in_progress" | "in-progress" => Some(TaskState::InProgress),
            "completed" => Some(TaskState::Completed),
            "abandoned" => Some(TaskState::Abandoned),
            _ => None,
        }
    }
}

/// One task inside a phase.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Task {
    pub id: String,
    pub text: String,
    #[serde(default)]
    pub state: TaskState,
}

/// A named phase containing zero or more tasks.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct Phase {
    pub name: String,
    #[serde(default)]
    pub tasks: Vec<Task>,
}

/// The whole todo list.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Todo {
    #[serde(default)]
    pub phases: Vec<Phase>,
}

impl Todo {
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace the entire phase list. Re-normalises afterwards.
    pub fn replace(&mut self, phases: Vec<Phase>) {
        self.phases = phases;
        self.normalise();
    }

    /// Append a new phase.
    pub fn add_phase(&mut self, name: impl Into<String>) {
        self.phases.push(Phase {
            name: name.into(),
            tasks: Vec::new(),
        });
    }

    /// Append a task to the phase at `phase_index`. Returns `false` if the
    /// index is out of range.
    pub fn add_task(&mut self, phase_index: usize, task: Task) -> bool {
        let Some(p) = self.phases.get_mut(phase_index) else {
            return false;
        };
        p.tasks.push(task);
        self.normalise();
        true
    }

    /// Update a task's state by id. Returns `false` if no such id exists.
    pub fn update(&mut self, id: &str, new_state: TaskState) -> bool {
        let mut found = false;
        for p in &mut self.phases {
            for t in &mut p.tasks {
                if t.id == id {
                    t.state = new_state;
                    found = true;
                    break;
                }
            }
            if found {
                break;
            }
        }
        if found {
            self.normalise();
        }
        found
    }

    /// Remove a task by id. Returns `false` if no such id exists.
    pub fn remove_task(&mut self, id: &str) -> bool {
        for p in &mut self.phases {
            let before = p.tasks.len();
            p.tasks.retain(|t| t.id != id);
            if p.tasks.len() != before {
                return true;
            }
        }
        false
    }

    /// Enforce "exactly one in-progress task" by walking phase-tasks in
    /// order: the first `InProgress` task is preserved, every later
    /// `InProgress` is demoted to `Pending`.
    pub fn normalise(&mut self) {
        let mut seen = false;
        for p in &mut self.phases {
            for t in &mut p.tasks {
                if matches!(t.state, TaskState::InProgress) {
                    if seen {
                        t.state = TaskState::Pending;
                    } else {
                        seen = true;
                    }
                }
            }
        }
    }

    /// Look up a task by id.
    pub fn find(&self, id: &str) -> Option<&Task> {
        self.phases
            .iter()
            .flat_map(|p| p.tasks.iter())
            .find(|t| t.id == id)
    }
}
