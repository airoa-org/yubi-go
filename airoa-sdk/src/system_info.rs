//! What software and hardware the rig is running, folded and de-duplicated
//! upstream. Every rule — which fields make a row unique, what order the rows
//! come back in, which rows are dropped — stays in `airoa_core.system_info`.

use serde::{Deserialize, Serialize};

use crate::Error;

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Software {
    pub name: String,
    pub version: String,
    pub vendor: String,
    pub description: String,
    pub git_uri: String,
    pub git_hash: String,
    pub git_branch: String,
    pub build_time: String,
    pub dependencies: Vec<String>,
    pub sub_namespace: String,
    pub role: String,
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Hardware {
    pub name: String,
    pub vendor: String,
    pub model: String,
    pub serial_number: String,
    pub firmware_version: String,
    pub firmware_build_version: String,
    pub driver_version: String,
    pub interface: String,
    pub extras: Vec<(String, String)>,
    pub sub_namespace: String,
}

/// `aggregate_software` — de-duplicate on (name, version, git_hash), drop
/// unnamed rows, and sort by name.
pub fn aggregate_software(items: &[Software]) -> Result<Vec<Software>, Error> {
    let v = crate::runtime::call("aggregate_software", serde_json::json!({"items": items}))?;
    serde_json::from_value(v).map_err(|e| Error::Python(format!("aggregate_software: {e}")))
}

/// `aggregate_hardware` — de-duplicate on (name, serial, firmware, build),
/// drop unnamed rows, and sort by name.
pub fn aggregate_hardware(items: &[Hardware]) -> Result<Vec<Hardware>, Error> {
    let v = crate::runtime::call("aggregate_hardware", serde_json::json!({"items": items}))?;
    serde_json::from_value(v).map_err(|e| Error::Python(format!("aggregate_hardware: {e}")))
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct Manifest {
    pub software: Vec<Software>,
    pub hardware: Vec<Hardware>,
}

/// Which YAML files [`load_manifest`] reads. An empty path is one not given;
/// several given paths concatenate. At least one is required.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ManifestPaths<'a> {
    /// A file with top-level `software:` / `hardware:` keys.
    pub combined: &'a str,
    /// A bare list of software entries.
    pub software: &'a str,
    /// A bare list of hardware entries.
    pub hardware: &'a str,
}

/// `load_manifest` — the version manifest, with blank fields filled from
/// `env`. YAML values always win; `env` only fills blanks, through upstream's
/// own `GIT_URI` / `GIT_HASH` / `GIT_BRANCH` / `BUILD_TIME` mapping.
///
/// The files are read *inside* the sandbox, so [`crate::add_preopen`] their
/// directory first and pass guest paths. `env` is passed explicitly because a
/// WASI guest has no environment of its own to fall back on. An entry with no
/// `name` is [`Error::Refused`].
pub fn load_manifest(paths: &ManifestPaths, env: &[(&str, &str)]) -> Result<Manifest, Error> {
    let env: serde_json::Map<String, serde_json::Value> = env
        .iter()
        .map(|(k, v)| ((*k).to_string(), serde_json::Value::String((*v).to_string())))
        .collect();
    let v = crate::runtime::call(
        "load_manifest",
        serde_json::json!({
            "combined_path": paths.combined,
            "software_path": paths.software,
            "hardware_path": paths.hardware,
            "env": env,
        }),
    )?;
    serde_json::from_value(v).map_err(|e| Error::Python(format!("load_manifest: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn up() {
        crate::start(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("build")).unwrap();
    }

    fn software(name: &str, version: &str) -> Software {
        Software { name: name.into(), version: version.into(), ..Software::default() }
    }

    #[test]
    fn software_folds_duplicates_drops_the_nameless_and_sorts() {
        up();
        let rows = aggregate_software(&[
            software("yubi", "1.0"),
            software("airoa_core", "2.0"),
            software("yubi", "1.0"),
            software("yubi", "1.1"),
            software("", "9.9"),
        ])
        .unwrap();
        let seen: Vec<(&str, &str)> =
            rows.iter().map(|s| (s.name.as_str(), s.version.as_str())).collect();
        assert_eq!(seen, [("airoa_core", "2.0"), ("yubi", "1.0"), ("yubi", "1.1")]);
    }

    #[test]
    fn hardware_folds_on_serial_and_firmware_not_on_the_rest() {
        up();
        let board = |serial: &str, firmware: &str, interface: &str| Hardware {
            name: "gripper".into(),
            serial_number: serial.into(),
            firmware_version: firmware.into(),
            interface: interface.into(),
            ..Hardware::default()
        };
        let rows = aggregate_hardware(&[
            board("L", "1.2", "usb"),
            // Same key, a different interface: still the same board upstream.
            board("L", "1.2", "uart"),
            board("R", "1.2", "usb"),
        ])
        .unwrap();
        let seen: Vec<&str> = rows.iter().map(|h| h.serial_number.as_str()).collect();
        assert_eq!(seen, ["L", "R"]);
        assert_eq!(rows[0].interface, "usb", "the first row of a key wins");
    }

    #[test]
    fn a_manifest_reads_from_the_mount_and_env_fills_only_blanks() {
        up();
        let base = std::env::temp_dir().join(format!("airoa_sdk_manifest-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        std::fs::write(
            base.join("versions.yaml"),
            "software:\n  - name: yubi-go\n    version: '0.1'\n    git_branch: main\n\
             hardware:\n  - name: gripper\n    vendor: airoa\n    model: g1\n",
        )
        .unwrap();
        crate::add_preopen(&base, "/manifest", crate::Access::ReadOnly);

        let paths = ManifestPaths { combined: "/manifest/versions.yaml", ..Default::default() };
        let manifest =
            load_manifest(&paths, &[("GIT_HASH", "abc123"), ("GIT_BRANCH", "ignored")]).unwrap();
        assert_eq!(manifest.software.len(), 1);
        assert_eq!(manifest.software[0].name, "yubi-go");
        assert_eq!(manifest.software[0].git_hash, "abc123", "env fills the blank");
        assert_eq!(manifest.software[0].git_branch, "main", "the file wins over env");
        assert_eq!(manifest.hardware[0].model, "g1");

        std::fs::write(base.join("nameless.yaml"), "software:\n  - version: '1'\n").unwrap();
        let paths = ManifestPaths { combined: "/manifest/nameless.yaml", ..Default::default() };
        assert!(matches!(load_manifest(&paths, &[]), Err(Error::Refused(_))));
    }
}
