use std::ffi::c_void;
use std::io;
use std::os::fd::{AsRawFd, OwnedFd};

const DISCONNECT_CLAIM: u32 = 0x8108_551B;
const SETINTERFACE: u32 = 0x8008_5504;
const CONTROL: u32 = 0xC018_5500;
const SUBMITURB: u32 = 0x8038_550A;
const REAPURB: u32 = 0x4008_550C;
const URB_TYPE_ISO: u8 = 0;
const URB_ISO_ASAP: u32 = 0x02;

/// Microframes per URB: 32 x 125 us is 4 ms of stream per transfer.
const PACKETS: usize = 32;

pub enum Packet<'a> {
    Data(&'a [u8], crate::misc::capture::UrbAt),
    Lost,
}

#[repr(C)]
struct CtrlTransfer {
    request_type: u8,
    request: u8,
    value: u16,
    index: u16,
    len: u16,
    timeout: u32,
    data: *mut u8,
}

#[repr(C)]
struct DisconnectClaim {
    interface: u32,
    flags: u32,
    driver: [u8; 256],
}

#[repr(C)]
struct SetInterface {
    interface: u32,
    alt_setting: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct IsoDesc {
    length: u32,
    actual_length: u32,
    status: u32,
}

#[repr(C)]
struct Urb {
    ep_type: u8,
    endpoint: u8,
    status: i32,
    flags: u32,
    buffer: *mut u8,
    buffer_length: i32,
    actual_length: i32,
    start_frame: i32,
    number_of_packets: u32,
    error_count: i32,
    signr: u32,
    usercontext: *mut c_void,
    iso: [IsoDesc; PACKETS],
}

pub struct Device {
    fd: OwnedFd,
    /// Boxed because the kernel holds each URB's address from submit to reap,
    /// and a `Vec<Urb>` would move them when it grows.
    #[allow(clippy::vec_box)]
    urbs: Vec<Box<Urb>>,
    _buffers: Vec<Box<[u8]>>,
    packet_size: usize,
}

impl Device {
    pub fn new(fd: OwnedFd) -> Self {
        Self { fd, urbs: Vec::new(), _buffers: Vec::new(), packet_size: 0 }
    }

    /// A second handle on the same open device, for control transfers from
    /// another thread: it owns no URBs, so unlike `Device` it can be sent.
    pub fn control_handle(&self) -> io::Result<ControlHandle> {
        Ok(ControlHandle(self.fd.try_clone()?))
    }

    fn ioctl<T>(&self, request: u32, arg: *mut T) -> io::Result<i32> {
        ioctl(&self.fd, request, arg)
    }

    pub fn descriptors(&self) -> io::Result<Vec<u8>> {
        let mut buf = vec![0u8; 4096];
        let n = unsafe {
            libc::pread(self.fd.as_raw_fd(), buf.as_mut_ptr() as *mut c_void, buf.len(), 0)
        };
        if n < 0 {
            return Err(io::Error::last_os_error());
        }
        buf.truncate(n as usize);
        Ok(buf)
    }

    /// Claims `interface`, first detaching any kernel driver bound to it.
    /// Horizon OS binds none, but stock Android (a Pixel) binds `uvcvideo` to
    /// every webcam, and a plain claim then fails `EBUSY`.
    pub fn claim_interface(&self, interface: u8) -> io::Result<()> {
        let mut arg = DisconnectClaim { interface: interface as u32, flags: 0, driver: [0; 256] };
        self.ioctl(DISCONNECT_CLAIM, &mut arg).map(drop)
    }

    pub fn set_alt_setting(&self, interface: u8, alt_setting: u8) -> io::Result<()> {
        let mut arg = SetInterface { interface: interface as u32, alt_setting: alt_setting as u32 };
        self.ioctl(SETINTERFACE, &mut arg).map(drop)
    }

    pub fn control(
        &self,
        request_type: u8,
        request: u8,
        value: u16,
        index: u16,
        data: &mut [u8],
    ) -> io::Result<usize> {
        control(&self.fd, request_type, request, value, index, data)
    }

    pub fn start_iso(&mut self, endpoint: u8, packet_size: usize, count: usize) -> io::Result<()> {
        self.packet_size = packet_size;
        for _ in 0..count {
            let mut buffer = vec![0u8; packet_size * PACKETS].into_boxed_slice();
            let mut urb = Box::new(Urb {
                ep_type: URB_TYPE_ISO,
                endpoint,
                status: 0,
                flags: URB_ISO_ASAP,
                buffer: buffer.as_mut_ptr(),
                buffer_length: buffer.len() as i32,
                actual_length: 0,
                start_frame: 0,
                number_of_packets: PACKETS as u32,
                error_count: 0,
                signr: 0,
                usercontext: std::ptr::null_mut(),
                iso: [IsoDesc { length: packet_size as u32, ..Default::default() }; PACKETS],
            });
            self.ioctl(SUBMITURB, &mut *urb)?;
            self._buffers.push(buffer);
            self.urbs.push(urb);
        }
        Ok(())
    }

    pub fn reap_iso(&mut self, mut on_packet: impl FnMut(Packet)) -> io::Result<()> {
        let mut ptr: *mut Urb = std::ptr::null_mut();
        self.ioctl(REAPURB, &mut ptr)?;
        let urb = unsafe { &mut *ptr };
        // One clock read per URB, not per packet: this is the reap thread.
        let reaped_ns = crate::misc::clock::boottime_ns();
        for i in 0..urb.number_of_packets as usize {
            let desc = urb.iso[i];
            if desc.status != 0 {
                on_packet(Packet::Lost);
            } else if desc.actual_length > 0 {
                let start = i * self.packet_size;
                let data = unsafe {
                    std::slice::from_raw_parts(urb.buffer.add(start), desc.actual_length as usize)
                };
                let at = crate::misc::capture::UrbAt { reaped_ns, packet: i as u16 };
                on_packet(Packet::Data(data, at));
            }
            urb.iso[i] = IsoDesc { length: self.packet_size as u32, ..Default::default() };
        }
        self.ioctl(SUBMITURB, urb).map(drop)
    }
}

pub struct ControlHandle(OwnedFd);

impl ControlHandle {
    pub fn control(
        &self,
        request_type: u8,
        request: u8,
        value: u16,
        index: u16,
        data: &mut [u8],
    ) -> io::Result<usize> {
        control(&self.0, request_type, request, value, index, data)
    }
}

fn ioctl<T>(fd: &OwnedFd, request: u32, arg: *mut T) -> io::Result<i32> {
    let r = unsafe { libc::ioctl(fd.as_raw_fd(), request as _, arg) };
    if r < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(r)
    }
}

fn control(
    fd: &OwnedFd,
    request_type: u8,
    request: u8,
    value: u16,
    index: u16,
    data: &mut [u8],
) -> io::Result<usize> {
    let mut arg = CtrlTransfer {
        request_type,
        request,
        value,
        index,
        len: data.len() as u16,
        timeout: 1000,
        data: data.as_mut_ptr(),
    };
    ioctl(fd, CONTROL, &mut arg).map(|n| n as usize)
}
