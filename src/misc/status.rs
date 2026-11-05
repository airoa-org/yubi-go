//! One `diagnostic_msgs/DiagnosticStatus`: how a part of the rig is doing, as
//! key/value text. The sensors and the hub fill these; the recorder writes
//! them to `/diagnostics` once a second and to `/yubi_go/events` as they happen.

pub const OK: u8 = 0;
pub const WARN: u8 = 1;
pub const ERROR: u8 = 2;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Status {
    pub level: u8,
    pub name: String,
    pub message: String,
    pub hardware_id: String,
    pub values: Vec<(String, String)>,
}

impl Status {
    pub fn new(name: impl Into<String>) -> Status {
        Status { name: name.into(), ..Status::default() }
    }

    pub fn with(mut self, key: &str, value: impl ToString) -> Status {
        self.values.push((key.to_string(), value.to_string()));
        self
    }
}
