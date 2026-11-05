use anyhow::Context as _;
use std::time::Duration;

use nusb::descriptors::{ConfigurationDescriptor, TransferType};
use nusb::transfer::{ControlOut, ControlType, Recipient};
use nusb::MaybeFuture;

const CDC_COMM: u8 = 0x02;
const CDC_DATA: u8 = 0x0a;

const SET_CONTROL_LINE_STATE: u8 = 0x22;

#[derive(Debug, Clone)]
pub struct AcmInfo {
    pub comm_interface: u8,
    pub data_interface: u8,
    pub data_alt_setting: u8,
    pub ep_in: u8,
    pub ep_max_packet: usize,
}

pub fn find_acm(config: &ConfigurationDescriptor) -> Option<AcmInfo> {
    let mut comm = None;
    let mut data = None;

    for intf in config.interface_alt_settings() {
        if intf.class() == CDC_COMM {
            comm.get_or_insert(intf.interface_number());
        } else if intf.class() == CDC_DATA {
            let ep = intf
                .endpoints()
                .find(|e| e.address() & 0x80 != 0 && e.transfer_type() == TransferType::Bulk);
            if let Some(ep) = ep {
                data.get_or_insert((
                    intf.interface_number(),
                    intf.alternate_setting(),
                    ep.address(),
                    ep.max_packet_size(),
                ));
            }
        }
    }

    let (data_interface, data_alt_setting, ep_in, ep_max_packet) = data?;
    Some(AcmInfo { comm_interface: comm?, data_interface, data_alt_setting, ep_in, ep_max_packet })
}

pub fn configure(comm: &nusb::Interface, info: &AcmInfo) -> anyhow::Result<()> {
    line_state(comm, info, 0x0003)
}

/// Reboot the ESP32 into its firmware the way esptool's hard reset does:
/// RTS alone pulls EN low, releasing both lets it boot with GPIO0 high. A
/// chip that enumerated but never spoke is usually sitting in the ROM
/// download mode a power dip left it in; a USB port reset does not touch
/// the CPU, this does.
pub fn reboot(comm: &nusb::Interface, info: &AcmInfo) -> anyhow::Result<()> {
    line_state(comm, info, 0x0002)?;
    std::thread::sleep(Duration::from_millis(100));
    line_state(comm, info, 0x0000)
}

/// CDC line state: bit 0 is DTR, bit 1 is RTS.
fn line_state(comm: &nusb::Interface, info: &AcmInfo, value: u16) -> anyhow::Result<()> {
    comm.control_out(
        ControlOut {
            control_type: ControlType::Class,
            recipient: Recipient::Interface,
            request: SET_CONTROL_LINE_STATE,
            value,
            index: info.comm_interface as u16,
            data: &[],
        },
        Duration::from_secs(1),
    )
    .wait()
    .context("SET_CONTROL_LINE_STATE")?;
    Ok(())
}

pub fn side_of(line: &str) -> Option<usize> {
    if line.starts_with('#') {
        return None;
    }
    let id = line.split(',').next()?;
    let count = |c: u8| id.bytes().filter(|b| b.eq_ignore_ascii_case(&c)).count();
    match (count(b'L'), count(b'R')) {
        (1, 0) => Some(0),
        (0, 1) => Some(1),
        _ => None,
    }
}

pub fn device_id(line: &str) -> Option<&str> {
    side_of(line)?;
    line.split(',').next()
}

#[derive(Debug, Default)]
pub struct SerialCounter {
    pub bytes: u64,
    pub lines: u64,
    partial: String,
}

impl SerialCounter {
    #[cfg(test)]
    pub fn push(&mut self, data: &[u8]) {
        self.push_with(data, |_| {})
    }

    pub fn push_with(&mut self, data: &[u8], mut on_line: impl FnMut(&str)) {
        self.bytes += data.len() as u64;
        self.partial.push_str(&String::from_utf8_lossy(data));
        while let Some(end) = self.partial.find('\n') {
            self.lines += 1;
            on_line(self.partial[..end].trim());
            self.partial.drain(..=end);
        }
        if self.partial.len() > 4096 {
            self.partial.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_side_letter_wherever_it_sits() {
        assert_eq!(side_of("L071,1.53"), Some(0));
        assert_eq!(side_of("R071,3.69"), Some(1));
        assert_eq!(side_of("PL004,2.36846634"), Some(0));
        assert_eq!(side_of("PR004,5.65885513"), Some(1));

        assert_eq!(side_of("846634"), None, "partial line at connect");
        assert_eq!(side_of("# AS5601 streaming: rad, device_id=PL004"), None, "banner");
        assert_eq!(side_of("LR001,1.0"), None, "ambiguous id must not be guessed");
        assert_eq!(side_of("0042,1.0"), None, "unlabelled id");
    }

    #[test]
    fn the_device_id_comes_from_lines_that_name_a_side() {
        assert_eq!(device_id("L071,1.53"), Some("L071"));
        assert_eq!(device_id("PR004,5.65885513"), Some("PR004"));
        assert_eq!(device_id("# AS5601 streaming: rad, device_id=PL004"), None, "banner");
        assert_eq!(device_id("0042,1.0"), None, "unlabelled id");
    }

    #[test]
    fn counts_bytes_and_lines() {
        let mut c = SerialCounter::default();
        c.push(b"0.42,1234,7\n0.43,12");
        c.push(b"36,8\n");
        assert_eq!(c.bytes, 24);
        assert_eq!(c.lines, 2);
    }
}
