use super::ui::{THUMB_H, THUMB_W};

pub fn thumbnail(jpeg: &[u8]) -> Option<Vec<u8>> {
    let mut decoder =
        zune_jpeg::JpegDecoder::new(zune_jpeg::zune_core::bytestream::ZCursor::new(jpeg));
    let rgb = decoder.decode().ok()?;
    let info = decoder.info()?;
    let (sw, sh) = (info.width as usize, info.height as usize);
    if sw == 0 || sh == 0 {
        return None;
    }
    let mut out = vec![0u8; THUMB_W * THUMB_H * 3];
    for y in 0..THUMB_H {
        let sy = y * sh / THUMB_H;
        for x in 0..THUMB_W {
            let sx = x * sw / THUMB_W;
            let s = (sy * sw + sx) * 3;
            let d = (y * THUMB_W + x) * 3;
            if s + 3 <= rgb.len() {
                out[d..d + 3].copy_from_slice(&rgb[s..s + 3]);
            }
        }
    }
    Some(out)
}
