//! The bytes of `signals.mcap`: a cursor over its records, and the CDR
//! messages the index reads.

use std::fs::File;
use std::io::{self, BufReader, Read};

use super::{Frame, ParameterSets};
use crate::record::mcap::GRIPPER_JOINTS;
use crate::record::mp4;

pub(super) const MAGIC: &[u8; 8] = b"\x89MCAP0\r\n";
pub(super) const OP_FOOTER: u8 = 0x02;
pub(super) const OP_CHANNEL: u8 = 0x04;
pub(super) const OP_MESSAGE: u8 = 0x05;
pub(super) const OP_CHUNK: u8 = 0x06;
pub(super) const OP_DATA_END: u8 = 0x0f;

/// A buffered reader that knows its file offset without asking the kernel.
pub(super) struct Cursor {
    pub(super) r: BufReader<File>,
    pub(super) pos: u64,
}

impl Cursor {
    pub(super) fn bytes(&mut self, n: usize) -> io::Result<Vec<u8>> {
        let mut buf = vec![0; n];
        self.r.read_exact(&mut buf)?;
        self.pos += n as u64;
        Ok(buf)
    }

    pub(super) fn u8(&mut self) -> io::Result<u8> {
        Ok(self.bytes(1)?[0])
    }

    pub(super) fn u64(&mut self) -> io::Result<u64> {
        Ok(u64::from_le_bytes(self.bytes(8)?.try_into().unwrap()))
    }

    pub(super) fn skip(&mut self, n: u64) -> io::Result<()> {
        self.r.seek_relative(n as i64)?;
        self.pos += n;
        Ok(())
    }
}

/// Little-endian CDR, aligned from the end of the 4-byte encapsulation header.
pub(super) struct Cdr<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Cdr<'a> {
    pub(super) fn new(buf: &'a [u8]) -> Cdr<'a> {
        Cdr { buf, pos: 4 }
    }

    fn align(&mut self, n: usize) {
        self.pos = 4 + (self.pos - 4).div_ceil(n) * n;
    }

    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let out = self.buf.get(self.pos..self.pos.checked_add(n)?)?;
        self.pos += n;
        Some(out)
    }

    fn u32(&mut self) -> Option<u32> {
        self.align(4);
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }

    fn u64(&mut self) -> Option<u64> {
        self.align(8);
        Some(u64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }

    fn f64(&mut self) -> Option<f64> {
        self.align(8);
        Some(f64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }

    /// A position then an orientation, which is a `Pose` and a `Transform`
    /// alike.
    pub(super) fn pose(&mut self) -> Option<[f64; 7]> {
        let mut out = [0.0; 7];
        for v in &mut out {
            *v = self.f64()?;
        }
        Some(out)
    }

    pub(super) fn text(&mut self) -> Option<&'a str> {
        let n = self.u32()? as usize;
        let s = self.take(n)?;
        std::str::from_utf8(s.strip_suffix(&[0]).unwrap_or(s)).ok()
    }

    pub(super) fn stamp(&mut self) -> Option<u64> {
        let sec = self.u32()? as u64;
        let nanosec = self.u32()? as u64;
        Some(sec * 1_000_000_000 + nanosec)
    }

    fn string(&mut self) -> Option<()> {
        let n = self.u32()? as usize;
        self.take(n).map(|_| ())
    }

    /// A `uint8[]`'s position in the payload and its length.
    fn bytes(&mut self) -> Option<(usize, usize)> {
        let n = self.u32()? as usize;
        let at = self.pos;
        self.take(n)?;
        Some((at, n))
    }
}

/// How many payload bytes a JPEG frame's header fields can span: the stamp,
/// a frame id of tens of bytes, `"jpeg"`, and the data length.
pub(super) const JPEG_HEAD: u64 = 256;

pub(super) fn jpeg_frame(head: &[u8], at: u64) -> Option<Frame> {
    let mut r = Cdr::new(head);
    let t_ns = r.stamp()?;
    r.string()?;
    r.string()?;
    let n = r.u32()?;
    Some(Frame { t_ns, offset: at + r.pos as u64, len: n, sample: 0, key: true })
}

pub(super) fn video_frame(body: &[u8], at: u64) -> Option<(Frame, Option<ParameterSets>)> {
    let mut r = Cdr::new(body);
    let t_ns = r.stamp()?;
    r.string()?;
    let (pos, n) = r.bytes()?;
    let data = &body[pos..pos + n];
    let key = mp4::is_key(data);
    let frame = Frame {
        t_ns,
        offset: at + pos as u64,
        len: n as u32,
        sample: mp4::avcc(data).len() as u32,
        key,
    };
    Some((frame, if key { mp4::parameter_sets(data) } else { None }))
}

/// A `UvcFrame`'s stamp, presentation time and first packet's reap time, if
/// the camera sent a presentation time.
pub(super) fn uvc_frame(body: &[u8]) -> Option<(u64, u32, u64)> {
    let mut r = Cdr::new(body);
    let t_ns = r.stamp()?;
    r.string()?;
    r.u64()?;
    let has_pts = *r.take(1)?.first()? != 0;
    let pts = r.u32()?;
    r.take(1)?;
    r.u64()?;
    let first = r.stamp()?;
    has_pts.then_some((t_ns, pts, first))
}

pub(super) fn joint_state(body: &[u8]) -> Option<(u64, f64)> {
    let mut r = Cdr::new(body);
    let t_ns = r.stamp()?;
    r.string()?;
    for _ in 0..r.u32()? {
        r.string()?;
    }
    if r.u32()? == 0 {
        return None;
    }
    Some((t_ns, r.f64()?))
}

/// Both grippers' angles, or nothing while either is uncalibrated: the
/// message names only the calibrated ones.
pub(super) fn joint_states(body: &[u8]) -> Option<(u64, [f64; 2])> {
    let mut r = Cdr::new(body);
    let t_ns = r.stamp()?;
    r.string()?;
    let n = r.u32()?;
    let names: Vec<&str> = (0..n).map(|_| r.text()).collect::<Option<_>>()?;
    if r.u32()? != n {
        return None;
    }
    let values: Vec<f64> = (0..n).map(|_| r.f64()).collect::<Option<_>>()?;
    let at = |joint: &str| Some(values[names.iter().position(|n| *n == joint)?]);
    Some((t_ns, [at(GRIPPER_JOINTS[0])?, at(GRIPPER_JOINTS[1])?]))
}

/// Every transform in a `TFMessage`, as `(child, stamp, pose)`.
pub(super) fn transforms(body: &[u8]) -> Vec<(&str, u64, [f64; 7])> {
    let mut r = Cdr::new(body);
    let Some(n) = r.u32() else { return Vec::new() };
    let mut out = Vec::new();
    for _ in 0..n {
        let one = (|| {
            let t_ns = r.stamp()?;
            r.string()?;
            Some((r.text()?, t_ns, r.pose()?))
        })();
        match one {
            Some(t) => out.push(t),
            None => break,
        }
    }
    out
}
