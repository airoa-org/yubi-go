use serde::{Deserialize, Deserializer, Serialize};

use super::lenient;

const PROFILE_FILE: &str = "robot_me.json";
const ASSIGNED_FILE: &str = "assigned.json";

/// A task reads as its name whichever way it arrives: a bare string from our
/// own cache, `{"name": ...}` from an enriched row.
fn task_name<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Named {
        Name(String),
        Object {
            #[serde(default, deserialize_with = "lenient")]
            name: String,
        },
        Absent,
    }
    Ok(match Option::<Named>::deserialize(deserializer)?.unwrap_or(Named::Absent) {
        Named::Name(name) => name,
        Named::Object { name } => name,
        Named::Absent => String::new(),
    })
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Profile {
    pub organization: String,
    pub site: String,
    pub location: String,
    pub robot_type: String,
    pub robot_id: String,
    /// The robot's name in the backend (`075`), which `stream::mdns` advertises.
    pub robot_name: String,
    pub operator: String,
}

impl Profile {
    pub fn is_named(&self) -> bool {
        !self.robot_id.is_empty()
    }

    pub fn save(&self, dir: &str) -> std::io::Result<()> {
        let body = serde_json::to_string(self).map_err(std::io::Error::other)?;
        std::fs::write(format!("{dir}/{PROFILE_FILE}"), body)
    }

    pub fn load(dir: &str) -> Option<Profile> {
        let body = std::fs::read_to_string(format!("{dir}/{PROFILE_FILE}")).ok()?;
        let profile: Profile = serde_json::from_str(&body).ok()?;
        profile.is_named().then_some(profile)
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Assignment {
    #[serde(alias = "episodeId", deserialize_with = "lenient")]
    pub id: String,
    /// The cache writes a name; airoa_core's enriched row carries the whole
    /// task object, and `/robot/episodes/repeat-last` is still raw wire.
    #[serde(alias = "task_name", deserialize_with = "task_name")]
    pub task: String,
    #[serde(alias = "taskId", deserialize_with = "lenient")]
    pub task_id: String,
    #[serde(alias = "taskVersionId", deserialize_with = "lenient")]
    pub task_version_id: String,
    #[serde(deserialize_with = "lenient")]
    pub subtasks: Vec<Subtask>,
    pub spent: bool,
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Subtask {
    #[serde(deserialize_with = "lenient")]
    pub id: String,
    #[serde(deserialize_with = "lenient")]
    pub name: String,
    #[serde(deserialize_with = "lenient")]
    pub status: u8,
}

impl Subtask {
    fn outstanding(&self) -> bool {
        self.status <= 1
    }
}

impl Assignment {
    pub fn current_subtask(&self) -> Option<(usize, &Subtask)> {
        let at = self
            .subtasks
            .iter()
            .position(|s| s.status == 1)
            .or_else(|| self.subtasks.iter().position(Subtask::outstanding))?;
        Some((at, &self.subtasks[at]))
    }

    pub fn instruction(&self) -> Option<&Subtask> {
        match self.subtasks.as_slice() {
            [only] => Some(only),
            _ => None,
        }
    }

    pub fn save_all(dir: &str, assigned: &[Assignment]) -> std::io::Result<()> {
        let cache = Self::cache(dir).unwrap_or_default();
        let spent: std::collections::HashSet<String> =
            cache.episodes.into_iter().filter(|a| a.spent).map(|a| a.id).collect();
        let episodes: Vec<Assignment> = assigned
            .iter()
            .map(|a| Assignment { spent: a.spent || spent.contains(&a.id), ..a.clone() })
            .collect();
        let last_task = assigned.first().map(|a| a.task.clone()).unwrap_or(cache.last_task);
        let body =
            serde_json::to_string(&Cache { episodes, last_task }).map_err(std::io::Error::other)?;
        std::fs::write(format!("{dir}/{ASSIGNED_FILE}"), body)
    }

    pub fn consume(dir: &str, id: &str) -> std::io::Result<()> {
        let Some(mut assigned) = Self::load_all(dir) else {
            return Ok(());
        };
        let Some(row) = assigned.iter_mut().find(|a| a.id == id) else {
            return Ok(());
        };
        if row.spent {
            return Ok(());
        }
        row.spent = true;
        Self::save_all(dir, &assigned)
    }

    pub fn load_all(dir: &str) -> Option<Vec<Assignment>> {
        Some(Self::cache(dir)?.episodes)
    }

    pub(super) fn cache(dir: &str) -> Option<Cache> {
        let body = std::fs::read_to_string(format!("{dir}/{ASSIGNED_FILE}")).ok()?;
        let mut cache: Cache = serde_json::from_str(&body).ok()?;
        cache.episodes.retain(|a| !a.id.is_empty());
        Some(cache)
    }
}

#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
pub(super) struct Cache {
    #[serde(deserialize_with = "lenient")]
    pub(super) episodes: Vec<Assignment>,
    #[serde(deserialize_with = "lenient")]
    pub(super) last_task: String,
}
