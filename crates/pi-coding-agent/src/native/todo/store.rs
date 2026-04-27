//! Persistence for [`super::Todo`]: `<cwd>/.pi/todo.json`.

use std::path::{Path, PathBuf};

use super::Todo;

/// Where the todo list lives for `cwd`.
pub fn todo_path(cwd: &Path) -> PathBuf {
    cwd.join(".pi").join("todo.json")
}

/// Load `<cwd>/.pi/todo.json`. Returns an empty [`Todo`] when the file
/// does not exist (callers shouldn't have to special-case "first run").
pub fn load(cwd: &Path) -> std::io::Result<Todo> {
    let path = todo_path(cwd);
    if !path.exists() {
        return Ok(Todo::new());
    }
    let raw = std::fs::read_to_string(&path)?;
    serde_json::from_str(&raw).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

/// Persist `todo` to `<cwd>/.pi/todo.json`, creating `.pi/` if needed.
pub fn save(cwd: &Path, todo: &Todo) -> std::io::Result<()> {
    let path = todo_path(cwd);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let txt = serde_json::to_string_pretty(todo)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(path, txt)
}
