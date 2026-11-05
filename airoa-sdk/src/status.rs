//! Reading a diagnostics array back as gate conditions — the inverse of
//! [`crate::gate::diagnostics`], and the shape [`crate::StatusInputs`] carries.

use crate::gate::Diagnostic;
use crate::Error;

/// `parse_diagnostics_for_gate` — rebuild the `gate_conditions` block from a
/// diagnostics array. Entries whose `hardware_id` is not `"recording_gate"`
/// are ignored, three-part names are per-condition and two-part names are the
/// group summary, and the two merge into one group.
///
/// The answer is `{"gate_level": …, "groups": {…}}`, or an empty object when
/// nothing in the array belonged to the gate. It stays untyped because it goes
/// straight into [`crate::StatusInputs::gate_conditions`], whose schema is
/// upstream's.
pub fn parse_for_gate(status: &[Diagnostic], gate_level: u8) -> Result<serde_json::Value, Error> {
    crate::runtime::call(
        "parse_diagnostics_for_gate",
        serde_json::json!({"status": status, "gate_level": gate_level}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gate::{Condition, Group, GroupState, KeyValue};
    use std::time::Duration;

    fn up() {
        crate::start(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("build")).unwrap();
    }

    /// The round trip is the point: what `gate::diagnostics` publishes is what
    /// this reads back, so a monitor recovers the tick the rig had.
    #[test]
    fn a_published_tick_reads_back_as_the_gate_conditions_it_came_from() {
        up();
        let mut states = vec![GroupState::new(
            Group { name: "health", settle: Duration::ZERO, recovery: Duration::ZERO },
            100.0,
        )];
        let conditions = vec![(
            "health".to_string(),
            vec![Condition {
                name: "cam".to_string(),
                passed: false,
                reason: "no data".to_string(),
                escalation: crate::gate::HARD_STOP,
                settle_exempt: false,
            }],
        )];
        let verdict =
            crate::gate::orchestrate(&mut states, &conditions, crate::gate::OK, 100.0).unwrap();
        let published = crate::gate::diagnostics(&states, &verdict, 100.0).unwrap();

        let read = parse_for_gate(&published, verdict.gate_level).unwrap();
        assert_eq!(read["gate_level"], verdict.gate_level);
        let health = &read["groups"]["health"];
        assert_eq!(health["level"], i32::from(crate::gate::HARD_STOP));
        assert_eq!(health["settled"], true);
        assert_eq!(health["conditions"][0]["name"], "cam");
        assert_eq!(health["conditions"][0]["passed"], false);
        assert_eq!(health["conditions"][0]["reason"], "no data");
        assert_eq!(health["conditions"][0]["escalation"], i32::from(crate::gate::HARD_STOP));
    }

    #[test]
    fn entries_from_another_hardware_id_are_no_gate_at_all() {
        up();
        let stranger = crate::gate::Diagnostic {
            name: "battery/level".to_string(),
            level: 2,
            message: "flat".to_string(),
            hardware_id: "battery".to_string(),
            values: vec![KeyValue { key: "pct".to_string(), value: "3".to_string() }],
        };
        assert_eq!(parse_for_gate(&[stranger], 2).unwrap(), serde_json::json!({}));
    }
}
