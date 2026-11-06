//! Which USB devices this rig has, as their descriptors said when they were
//! opened.
//!
//! The JNI scan is the only code that ever sees a descriptor, and the only
//! moment `getSerialNumber` is legal is right after the permission check. So
//! it leaves what it read here, and meta.json's device list reads it back
//! when a take closes — instead of threading five strings through the camera
//! and gripper workers to the recorder.

use std::path::Path;
use std::sync::Mutex;

use crate::hub::USB_CAM_SLOTS;

/// What a device is to this rig, decided from its interface descriptors.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Kind {
    Video,
    Serial,
}

const CLASS_CDC_COMM: u8 = 0x02;
const CLASS_VIDEO: u8 = 0x0e;
const CLASS_BILLBOARD: u8 = 0x11;
/// CDC subclass for a serial port — the ESP32-C6's USB-Serial/JTAG is one.
const SUBCLASS_ACM: u8 = 0x02;

/// `(class, subclass)` of each interface → what to open it as, or nothing.
///
/// A serial device is a CDC *ACM* device. The CDC-Data class alone (0x0a)
/// also matches the USB-C hub's LAN adapter, which is CDC NCM: the same data
/// class under a different comm subclass.
pub fn classify(interfaces: impl IntoIterator<Item = (u8, u8)>) -> Option<Kind> {
    let mut kind = None;
    for (class, subclass) in interfaces {
        match (class, subclass) {
            (CLASS_VIDEO, _) => return Some(Kind::Video),
            // The hub's own Billboard device also carries a CDC data
            // interface; it is not an encoder, and resetting it resets the hub.
            (CLASS_BILLBOARD, _) => return None,
            (CLASS_CDC_COMM, SUBCLASS_ACM) => kind = Some(Kind::Serial),
            _ => {}
        }
    }
    kind
}

/// A descriptor string as a person would write it. Firmware pads these with
/// NUL — the encoder's manufacturer reads `Espressif\0` — and a NUL in JSON
/// is an escape nobody wants to grep for.
pub fn descriptor_text(raw: &str) -> String {
    raw.trim_matches(|c: char| c == '\0' || c.is_whitespace()).to_string()
}

/// One device as the scan saw it. Plain strings, no SDK type: what the rig
/// has is a fact about the rig, and how upstream spells it is
/// `record::system_info`'s to decide.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UsbDevice {
    pub name: String,
    pub vendor: String,
    /// `vvvv:pppp`.
    pub model: String,
    pub serial: String,
    /// `usb-uvc` or `usb-cdc`.
    pub interface: String,
    /// Android's `/dev/bus/usb/BBB/DDD`. A replug renumbers it, so it only
    /// says whether this row's device is still attached.
    pub path: String,
    /// The hub port (`port_name`), empty when the kernel would not say. The
    /// one thing that tells three identical webcams apart across replugs when
    /// none of them carries a serial number.
    pub port: String,
}

/// The hub port a device hangs off — sysfs's `1-1.2.6` — for the device at
/// Android's `/dev/bus/usb/BBB/DDD`. The address is reissued on every
/// replug; the port is where the cable is, and it is the only identity three
/// identical webcams without serial numbers have.
pub fn port_chain(devpath: &str) -> Option<String> {
    port_chain_in(Path::new("/sys/bus/usb/devices"), devpath)
}

fn port_chain_in(sysfs: &Path, devpath: &str) -> Option<String> {
    let mut tail = devpath.rsplit('/');
    let dev: u32 = tail.next()?.parse().ok()?;
    let bus: u32 = tail.next()?.parse().ok()?;
    let number = |dir: &Path, file: &str| -> Option<u32> {
        std::fs::read_to_string(dir.join(file)).ok()?.trim().parse().ok()
    };
    std::fs::read_dir(sysfs).ok()?.filter_map(Result::ok).find_map(|entry| {
        let dir = entry.path();
        (number(&dir, "busnum") == Some(bus) && number(&dir, "devnum") == Some(dev))
            .then(|| entry.file_name().to_string_lossy().into_owned())
    })
}

const PORTS_FILE: &str = "camera_ports.json";
static PORTS: Mutex<()> = Mutex::new(());

/// The rank of the camera at port `chain`: its place in `camera_ports.json`,
/// which lists ports in the order they were first seen. `camera_roles` names
/// ranks, so a rank must mean the same port across replugs and restarts. None
/// when every rank belongs to another port, because the hub was rewired.
pub fn rank_of(dir: &str, chain: &str) -> Option<usize> {
    let _guard = PORTS.lock().unwrap_or_else(|e| e.into_inner());
    let path = Path::new(dir).join(PORTS_FILE);
    let mut table: Vec<String> = std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default();
    if let Some(rank) = table.iter().position(|port| port == chain) {
        // A table edited by hand can list more ports than there are ranks.
        return (rank < USB_CAM_SLOTS.len()).then_some(rank);
    }
    if table.len() >= USB_CAM_SLOTS.len() {
        return None;
    }
    table.push(chain.to_string());
    // A half-written table reads as empty, and every rank is handed out anew.
    if let Err(e) = crate::misc::save::replace(&path, &serde_json::to_vec(&table).ok()?) {
        log::warn!("{}: {e} — camera ranks will not survive a restart", path.display());
    }
    Some(table.len() - 1)
}

static SEEN: Mutex<Vec<UsbDevice>> = Mutex::new(Vec::new());

pub fn note(device: UsbDevice) {
    merge(&mut SEEN.lock().unwrap_or_else(|e| e.into_inner()), device);
}

/// Every scan says which addresses are attached; a row whose address is gone
/// was unplugged, and a device that comes back gets a new address anyway.
pub fn retain(present_paths: &[String]) {
    prune(&mut SEEN.lock().unwrap_or_else(|e| e.into_inner()), present_paths);
}

/// Whether the last scan still found the device at `path`.
pub fn attached(path: &str) -> bool {
    SEEN.lock().unwrap_or_else(|e| e.into_inner()).iter().any(|d| d.path == path)
}

pub fn seen() -> Vec<UsbDevice> {
    SEEN.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// What this file is called, next to the episodes.
pub const EVENT_LOG: &str = "usb-events.log";

/// One line per open and close, kept on disk, because logcat is gone after a
/// reboot and how often the hub drops a device has to be counted across them.
pub fn event(dir: &str, line: &str) {
    use std::io::Write;
    /// ~10,000 lines of ~100 bytes. The one before it is kept as `.1`.
    const ROTATE_AT: u64 = 1 << 20;
    static ROTATING: Mutex<()> = Mutex::new(());
    let stamp = crate::misc::clock::rfc3339(crate::misc::clock::realtime_ns());
    let path = Path::new(dir).join(EVENT_LOG);
    let _guard = ROTATING.lock().unwrap_or_else(|e| e.into_inner());
    if std::fs::metadata(&path).is_ok_and(|m| m.len() >= ROTATE_AT) {
        if let Err(e) = std::fs::rename(&path, path.with_extension("log.1")) {
            log::warn!("usb: {EVENT_LOG} not rotated: {e}");
        }
    }
    let file = std::fs::OpenOptions::new().create(true).append(true).open(&path);
    // One write per line: two workers closing at once must not interleave.
    if let Err(e) = file.and_then(|mut f| f.write_all(format!("{stamp} {line}\n").as_bytes())) {
        log::warn!("usb: {EVENT_LOG} not written: {e}");
    }
}

/// A device's physical place, `bus-port.port…`, named as Linux names it under
/// `/sys/bus/usb/devices`. Unlike `/dev/bus/usb/BBB/DDD`, which a replug
/// renumbers, it stays put while the cable stays in the same hub port.
pub fn port_name(bus: u32, ports: &[u8]) -> String {
    let chain: Vec<String> = ports.iter().map(u8::to_string).collect();
    format!("{bus}-{}", chain.join("."))
}

/// A rescan re-opens the same device at the same address; replace the row,
/// don't stack it. Keyed by address rather than name, because three cameras of
/// one model all answer to the same name.
fn merge(rows: &mut Vec<UsbDevice>, device: UsbDevice) {
    match rows.iter_mut().find(|d| d.path == device.path) {
        Some(row) => *row = device,
        None => rows.push(device),
    }
}

fn prune(rows: &mut Vec<UsbDevice>, present_paths: &[String]) {
    rows.retain(|d| present_paths.contains(&d.path));
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_port_is_named_as_sysfs_names_it() {
        assert_eq!(super::port_name(1, &[1, 3]), "1-1.3");
        assert_eq!(super::port_name(2, &[4]), "2-4");
    }

    #[test]
    fn events_append_one_stamped_line_each() {
        let dir = std::env::temp_dir().join(format!("yubigo-usb-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let dir = dir.to_str().unwrap();
        super::event(dir, "opened cam A (Video)");
        super::event(dir, "[cam A] closed: reap: No such device");
        let text =
            std::fs::read_to_string(std::path::Path::new(dir).join(super::EVENT_LOG)).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].ends_with("Z opened cam A (Video)"), "{}", lines[0]);
        assert!(lines[1].contains("No such device"));
    }

    use super::*;

    fn dev(path: &str, name: &str, serial: &str) -> UsbDevice {
        UsbDevice {
            path: path.into(),
            name: name.into(),
            serial: serial.into(),
            ..Default::default()
        }
    }

    /// Through `merge`, not `note`: filling the global from a test would put
    /// a phantom device into every episode the rest of the suite records.
    #[test]
    fn a_rescan_replaces_a_device_rather_than_stacking_it() {
        let mut rows = Vec::new();
        merge(&mut rows, dev("001/002", "cam", "S1"));
        merge(&mut rows, dev("001/003", "grip", "G1"));
        merge(&mut rows, dev("001/002", "cam", "S2"));
        assert_eq!(rows, vec![dev("001/002", "cam", "S2"), dev("001/003", "grip", "G1")]);
    }

    #[test]
    fn three_cameras_of_one_model_are_three_rows() {
        let mut rows = Vec::new();
        for path in ["001/002", "001/003", "001/004"] {
            merge(&mut rows, dev(path, "USB Camera", ""));
        }
        assert_eq!(rows.len(), 3, "{rows:?}");
    }

    #[test]
    fn a_cdc_acm_device_is_serial_and_a_lan_adapter_is_nothing() {
        assert_eq!(classify([(0x02, 0x02), (0x0a, 0x00)]), Some(Kind::Serial), "ESP32-C6");
        assert_eq!(classify([(0x02, 0x0d), (0x0a, 0x00)]), None, "CDC NCM LAN adapter");
        assert_eq!(classify([(0x11, 0x00), (0x02, 0x02), (0x0a, 0x00)]), None, "hub Billboard");
    }

    fn sysfs_with(entries: &[(&str, Option<(u32, u32)>)]) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!("yubigo_sysfs_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for (name, numbers) in entries {
            let dir = root.join(name);
            std::fs::create_dir_all(&dir).unwrap();
            if let Some((bus, dev)) = numbers {
                std::fs::write(dir.join("busnum"), format!("{bus}\n")).unwrap();
                std::fs::write(dir.join("devnum"), format!("{dev}\n")).unwrap();
            }
        }
        root
    }

    #[test]
    fn a_device_address_resolves_to_its_hub_port() {
        let sysfs = sysfs_with(&[
            ("1-1", Some((1, 2))),
            ("1-1.2", Some((1, 3))),
            ("1-1.2:1.0", None),
            ("1-1.2.1", Some((1, 5))),
            ("1-1.3", Some((1, 4))),
            ("usb1", Some((1, 1))),
        ]);
        assert_eq!(port_chain_in(&sysfs, "/dev/bus/usb/001/005").as_deref(), Some("1-1.2.1"));
        assert_eq!(port_chain_in(&sysfs, "/dev/bus/usb/001/004").as_deref(), Some("1-1.3"));
        assert_eq!(port_chain_in(&sysfs, "/dev/bus/usb/002/004"), None, "wrong bus");
        assert_eq!(port_chain_in(&sysfs, "garbage"), None);
    }

    #[test]
    fn a_hub_port_keeps_its_rank_across_a_replug_and_a_restart() {
        let dir = std::env::temp_dir().join(format!("yubigo_ports_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let dir = dir.to_str().unwrap();
        assert_eq!(rank_of(dir, "1-1.3"), Some(0), "first seen takes rank 0");
        assert_eq!(rank_of(dir, "1-1.2.1"), Some(1));
        assert_eq!(rank_of(dir, "1-1.2.6"), Some(2));
        // The replug enumerates in another order; the ranks do not move.
        assert_eq!(rank_of(dir, "1-1.2.6"), Some(2));
        assert_eq!(rank_of(dir, "1-1.3"), Some(0));
        assert_eq!(rank_of(dir, "1-1.4"), None, "a fourth port has no rank to take");
        let saved: Vec<String> =
            serde_json::from_str(&std::fs::read_to_string(format!("{dir}/{PORTS_FILE}")).unwrap())
                .unwrap();
        assert_eq!(saved, ["1-1.3", "1-1.2.1", "1-1.2.6"], "what a restart reads back");

        std::fs::write(format!("{dir}/{PORTS_FILE}"), r#"["a", "b", "c", "1-1.4"]"#).unwrap();
        assert_eq!(rank_of(dir, "1-1.4"), None, "a fourth entry is no rank");
        assert_eq!(rank_of(dir, "c"), Some(2));
    }
}
