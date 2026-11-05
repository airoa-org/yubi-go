//! What made this take: the software and hardware behind meta.json's
//! `programs` and `devices`.
//!
//! The host half of `airoa_sdk::system_info`, and named after it — upstream
//! calls this system info, and inventing a second word for it would only make
//! the two harder to line up.
//!
//! ADC (`airoa_data_collection`, AIRoA's ROS data collection stack) discovers
//! this from the live ROS service graph; YUBI Go has no graph,
//! so the rows come from the three places it can actually know: what the
//! binary knows about itself, what `misc::usb` kept from each descriptor when
//! the device was opened, and a static manifest for the parts that cannot
//! report themselves (upstream's `version_manifest.yaml`, next to
//! config.json). Folding and de-duplication stay upstream, in
//! `airoa_core.system_info`.

use std::path::Path;

use airoa_sdk::system_info::{
    aggregate_hardware, aggregate_software, load_manifest, Hardware, Manifest, ManifestPaths,
    Software,
};

/// Upstream's roles for rows nobody declared by hand. A row that names its
/// own role (this binary, or a manifest entry) keeps it.
const SOFTWARE_ROLE: &str = "discovered_software";
const HARDWARE_ROLE: &str = "discovered_hardware";

/// Read from the files dir, through the same guest mount the uploader uses.
/// Absent is the normal case.
pub const MANIFEST: &str = "version_manifest.yaml";

const OUR_ROLES: [&str; 2] = ["interface", "data_collection"];

fn ours(role: &str) -> Software {
    Software {
        name: "yubi-go".into(),
        git_uri: env!("YUBIGO_GIT_URI").into(),
        git_hash: env!("YUBIGO_GIT_HASH").into(),
        git_branch: env!("YUBIGO_GIT_BRANCH").into(),
        role: role.into(),
        ..Software::default()
    }
}

/// Upstream's `default_software_info`: the canonical repository, the commit
/// the build stamped in, and `unknown` for what it did not. The stamp is the
/// running payload's own answer, which names the commits it was built from.
fn core() -> Software {
    let unknown = || "unknown".to_string();
    Software {
        name: "airoa_core".into(),
        git_uri: "https://github.com/airoa-org/airoa_data_collection".into(),
        git_hash: airoa_sdk::payload().get("airoa_core_commit").cloned().unwrap_or_else(unknown),
        git_branch: unknown(),
        ..Software::default()
    }
}

fn manifest(dir: &str) -> Manifest {
    if !Path::new(dir).join(MANIFEST).exists() {
        return Manifest::default();
    }
    let guest = format!("{}/{MANIFEST}", crate::record::upload::GUEST_ROOT);
    let paths = ManifestPaths { combined: &guest, ..ManifestPaths::default() };
    // Blank git fields in the file inherit this build's, the way upstream's
    // publisher node fills them from the environment.
    let env = [
        ("GIT_URI", env!("YUBIGO_GIT_URI")),
        ("GIT_HASH", env!("YUBIGO_GIT_HASH")),
        ("GIT_BRANCH", env!("YUBIGO_GIT_BRANCH")),
    ];
    match load_manifest(&paths, &env) {
        Ok(manifest) => manifest,
        Err(e) => {
            log::warn!("{MANIFEST} ignored — {e}");
            Manifest::default()
        }
    }
}

/// The `programs` and `devices` rows to add to one episode's meta.json.
/// `dir` is the files dir, where the manifest lives.
pub fn discovered(dir: &str) -> (Vec<serde_json::Value>, Vec<serde_json::Value>) {
    let manifest = manifest(dir);

    let mut software = vec![core()];
    software.extend(manifest.software);
    let software = aggregate_software(&software).unwrap_or_else(|e| {
        log::warn!("software rows unfolded — {e}");
        software
    });
    let mut programs: Vec<serde_json::Value> =
        OUR_ROLES.iter().map(|role| program(&ours(role))).collect();
    programs.extend(software.iter().map(program));

    let mut hardware: Vec<Hardware> = crate::misc::usb::seen().iter().map(hardware_row).collect();
    hardware.extend(manifest.hardware);
    let hardware = aggregate_hardware(&hardware).unwrap_or_else(|e| {
        log::warn!("hardware rows unfolded — {e}");
        hardware
    });

    (programs, hardware.iter().map(device).collect())
}

/// Upstream's row for a device the USB scan opened. Firmware and driver stay
/// blank on purpose: nothing on this rig reports a firmware version, and the
/// driver is this binary, which is already a program row.
///
/// A device with no serial number gets its port there instead. Upstream folds
/// rows on `(name, serial)`, so three identical cameras with blank serials
/// would come out of `aggregate_hardware` as one. The hub port makes them
/// three, and stays the same across a replug into the same port. Without it,
/// the `/dev/bus/usb/…` address still makes them three for this take.
fn hardware_row(d: &crate::misc::usb::UsbDevice) -> Hardware {
    Hardware {
        name: d.name.clone(),
        vendor: d.vendor.clone(),
        model: d.model.clone(),
        serial_number: [&d.serial, &d.port, &d.path]
            .into_iter()
            .find(|s| !s.is_empty())
            .cloned()
            .unwrap_or_default(),
        interface: d.interface.clone(),
        ..Default::default()
    }
}

/// `SoftwareInfo.to_metadata_program` — a `ProgramV2_0` has no slot for
/// vendor, description, dependencies or build time, so those are dropped.
fn program(s: &Software) -> serde_json::Value {
    let name =
        if s.version.is_empty() { s.name.clone() } else { format!("{}@{}", s.name, s.version) };
    let role = if s.role.is_empty() { SOFTWARE_ROLE } else { &s.role };
    serde_json::json!({
        "role": role,
        "name": name,
        "source": {"git": {
            "uri": s.git_uri, "hash": s.git_hash, "branch": s.git_branch, "tag": null,
        }},
    })
}

/// `HardwareInfo.to_metadata_device` — a `DeviceV2_0` is three strings, so
/// vendor/model go into `type` and the rest is packed into `id`.
fn device(h: &Hardware) -> serde_json::Value {
    let kind = match [h.vendor.as_str(), h.model.as_str()]
        .iter()
        .filter(|p| !p.is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join("/")
    {
        empty if empty.is_empty() => "unknown".to_string(),
        kind => kind,
    };
    serde_json::json!({"role": HARDWARE_ROLE, "type": kind, "id": packed_id(h)})
}

/// Upstream's display-only packing: `name#serial@fw=…;build=…;drv=…;if=…`.
/// Nothing parses it back, so a value carrying one of the separators reads
/// oddly but breaks nothing.
fn packed_id(h: &Hardware) -> String {
    let mut id = if h.name.is_empty() { "unknown".to_string() } else { h.name.clone() };
    if !h.serial_number.is_empty() {
        id += &format!("#{}", h.serial_number);
    }
    let suffix: Vec<String> = [
        ("fw", &h.firmware_version),
        ("build", &h.firmware_build_version),
        ("drv", &h.driver_version),
        ("if", &h.interface),
    ]
    .iter()
    .filter(|(_, v)| !v.is_empty())
    .map(|(k, v)| format!("{k}={v}"))
    .collect();
    if !suffix.is_empty() {
        id += &format!("@{}", suffix.join(";"));
    }
    id
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_row_maps_the_way_upstream_maps_it() {
        let h = Hardware {
            name: "lidar_front".into(),
            vendor: "Velodyne".into(),
            model: "VLP-16".into(),
            serial_number: "X1".into(),
            firmware_version: "3.1.4".into(),
            interface: "ethernet".into(),
            ..Hardware::default()
        };
        assert_eq!(
            device(&h),
            serde_json::json!({
                "role": "discovered_hardware",
                "type": "Velodyne/VLP-16",
                "id": "lidar_front#X1@fw=3.1.4;if=ethernet",
            })
        );
        // Nothing but a name: no separators, and a type that still reads.
        let bare = Hardware { name: "cam".into(), ..Hardware::default() };
        assert_eq!(device(&bare)["type"], "unknown");
        assert_eq!(device(&bare)["id"], "cam");

        let s = Software { name: "yubi-go".into(), version: "0.1.0".into(), ..Software::default() };
        assert_eq!(program(&s)["name"], "yubi-go@0.1.0");
        assert_eq!(program(&s)["role"], "discovered_software");
        let unversioned = Software { name: "yubi-go".into(), ..Software::default() };
        assert_eq!(program(&unversioned)["name"], "yubi-go", "no version, no @");
    }

    #[test]
    fn a_camera_without_a_serial_is_named_by_its_hub_port() {
        let d = |serial: &str, port: &str| crate::misc::usb::UsbDevice {
            name: "USB Camera".into(),
            serial: serial.into(),
            port: port.into(),
            path: "/dev/bus/usb/001/007".into(),
            ..Default::default()
        };
        assert_eq!(hardware_row(&d("S1", "1-1.2.6")).serial_number, "S1");
        assert_eq!(hardware_row(&d("", "1-1.2.6")).serial_number, "1-1.2.6");
        assert_eq!(hardware_row(&d("", "")).serial_number, "/dev/bus/usb/001/007");
    }
}
