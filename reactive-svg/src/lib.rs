//! React's model for one RGBA texture. A frame `render`s a list of keyed
//! elements; an element equal to the one committed under its key last time
//! bails out, so only what changed is rasterized, composited and handed back
//! as patches.
//!
//! An element is a fragment of SVG in the root's own coordinates, or a raw RGB
//! image. Elements composite in list order, later over earlier, and each one's
//! box is what resvg reports for it, so nobody hand-writes a dirty rect.
//!
//! Elements are what components return: a function per part of the screen,
//! keyed by its place in the tree (`"sidebar/clock"`), so a changed number
//! redraws its own component and leaves the chrome around it cached.

mod signal;
pub use signal::{signal, Getter, Setter};

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

pub use resvg;
pub use resvg::usvg::fontdb;
use resvg::{tiny_skia, usvg};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: usize,
    pub y: usize,
    pub w: usize,
    pub h: usize,
}

impl Rect {
    fn intersect(self, o: Rect) -> Option<Rect> {
        let (x, y) = (self.x.max(o.x), self.y.max(o.y));
        let (r, b) = ((self.x + self.w).min(o.x + o.w), (self.y + self.h).min(o.y + o.h));
        (r > x && b > y).then(|| Rect { x, y, w: r - x, h: b - y })
    }
}

/// What changed: straight-alpha RGBA for `rect`, rows top-down.
pub struct Patch {
    pub rect: Rect,
    pub rgba: Vec<u8>,
}

impl Patch {
    /// Copy into a `w`-wide top-down RGBA buffer.
    pub fn paste(&self, buf: &mut [u8], w: usize) {
        let r = self.rect;
        for (row, src) in self.rgba.chunks_exact(r.w * 4).enumerate() {
            buf[((r.y + row) * w + r.x) * 4..][..r.w * 4].copy_from_slice(src);
        }
    }
}

pub struct Element<'a> {
    key: String,
    source: Source<'a>,
}

#[derive(PartialEq)]
enum Source<'a> {
    Svg(String),
    Rgb(Rect, &'a [u8]),
}

/// A fragment of SVG: any elements a `<svg>` body may hold, in root pixels.
pub fn svg(key: impl Into<String>, markup: impl Into<String>) -> Element<'static> {
    Element { key: key.into(), source: Source::Svg(markup.into()) }
}

/// Packed RGB, `rect.w * rect.h * 3` bytes, opaque. The part outside the
/// root is clipped; a buffer of any other length draws nothing.
pub fn image(key: impl Into<String>, rect: Rect, rgb: &[u8]) -> Element<'_> {
    Element { key: key.into(), source: Source::Rgb(rect, rgb) }
}

/// One committed element: what it was made from, and its pixels.
struct Fiber {
    key: String,
    made_from: Made,
    /// Where it drew, inside the root; `None` when it drew nothing.
    rect: Option<Rect>,
    /// Premultiplied RGBA, `rect`-sized.
    pixels: Vec<u8>,
}

enum Made {
    Svg(String),
    Rgb(Rect, Vec<u8>),
}

impl Fiber {
    fn same(&self, el: &Element) -> bool {
        match (&self.made_from, &el.source) {
            (Made::Svg(was), Source::Svg(m)) => was == m,
            (Made::Rgb(r0, was), Source::Rgb(r, rgb)) => r0 == r && was == rgb,
            _ => false,
        }
    }
}

pub struct Root {
    w: usize,
    h: usize,
    head: String,
    options: usvg::Options<'static>,
    fibers: Vec<Fiber>,
    /// Premultiplied RGBA, the whole root.
    canvas: Vec<u8>,
}

impl Root {
    /// `head` goes before every fragment: the `<defs>` and `<style>` they share.
    pub fn new(w: usize, h: usize, head: impl Into<String>, fonts: Arc<fontdb::Database>) -> Self {
        let options = usvg::Options { fontdb: fonts, ..Default::default() };
        Self { w, h, head: head.into(), options, fibers: Vec::new(), canvas: vec![0; w * h * 4] }
    }

    /// Commit `elements` and return what changed. A key that comes twice
    /// keeps its first element; the later ones are logged and dropped.
    pub fn render(&mut self, elements: Vec<Element>) -> Vec<Patch> {
        let mut seen = HashSet::new();
        let elements: Vec<Element> = elements
            .into_iter()
            .filter(|el| {
                let first = seen.insert(el.key.clone());
                if !first {
                    log::error!(
                        "reactive-svg: key {:?} given twice; the later one dropped",
                        el.key
                    );
                }
                first
            })
            .collect();
        // Kept elements in their old and new order. One that keeps its place
        // among them composites as before; one that comes or goes dirties its
        // own rect.
        let was: Vec<String> =
            self.fibers.iter().map(|f| f.key.clone()).filter(|k| seen.contains(k)).collect();
        let mut kept = 0;
        let mut dirty = Vec::new();
        let mut old: HashMap<String, Fiber> =
            self.fibers.drain(..).map(|f| (f.key.clone(), f)).collect();
        for el in elements {
            let fiber = match old.remove(&el.key) {
                Some(f) => {
                    let moved = was.get(kept) != Some(&f.key);
                    kept += 1;
                    if f.same(&el) {
                        if moved {
                            dirty.extend(f.rect);
                        }
                        f
                    } else {
                        dirty.extend(f.rect);
                        let f = self.mount(el);
                        dirty.extend(f.rect);
                        f
                    }
                }
                None => {
                    let f = self.mount(el);
                    dirty.extend(f.rect);
                    f
                }
            };
            self.fibers.push(fiber);
        }
        dirty.extend(old.into_values().filter_map(|f| f.rect));
        dirty.sort_by_key(|r| (r.y, r.x, r.h, r.w));
        dirty.dedup();
        dirty.into_iter().map(|r| self.composite(r)).collect()
    }

    fn mount(&self, el: Element) -> Fiber {
        let (drawn, made_from) = match el.source {
            Source::Rgb(r, rgb) => (self.opaque(r, rgb), Made::Rgb(r, rgb.to_vec())),
            Source::Svg(markup) => (self.rasterize(&markup), Made::Svg(markup)),
        };
        let (rect, pixels) = drawn.unzip();
        Fiber { key: el.key, made_from, rect, pixels: pixels.unwrap_or_default() }
    }

    /// `rgb` as premultiplied RGBA, clipped to the root.
    fn opaque(&self, r: Rect, rgb: &[u8]) -> Option<(Rect, Vec<u8>)> {
        if rgb.len() != r.w * r.h * 3 {
            log::error!("reactive-svg: {} RGB bytes for a {}x{} image", rgb.len(), r.w, r.h);
            return None;
        }
        let clip = r.intersect(Rect { x: 0, y: 0, w: self.w, h: self.h })?;
        let mut px = Vec::with_capacity(clip.w * clip.h * 4);
        for y in clip.y..clip.y + clip.h {
            let row = &rgb[((y - r.y) * r.w + clip.x - r.x) * 3..][..clip.w * 3];
            px.extend(row.as_chunks::<3>().0.iter().flat_map(|&[r, g, b]| [r, g, b, 255]));
        }
        Some((clip, px))
    }

    fn rasterize(&self, markup: &str) -> Option<(Rect, Vec<u8>)> {
        let doc = format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="{}" height="{}">{}{markup}</svg>"#,
            self.w, self.h, self.head
        );
        let tree = usvg::Tree::from_data(doc.as_bytes(), &self.options)
            .map_err(|e| log::error!("reactive-svg: {e}"))
            .ok()?;
        let root = tree.root();
        if root.children().is_empty() {
            return None;
        }
        let b = root.abs_layer_bounding_box();
        let (x, y) = (b.left().floor().max(0.0) as usize, b.top().floor().max(0.0) as usize);
        let (r, bottom) = (b.right().ceil().max(0.0) as usize, b.bottom().ceil().max(0.0) as usize);
        let full = Rect { x: 0, y: 0, w: self.w, h: self.h };
        let r =
            Rect { x, y, w: r.saturating_sub(x), h: bottom.saturating_sub(y) }.intersect(full)?;
        let mut pm = tiny_skia::Pixmap::new(r.w as u32, r.h as u32)?;
        let at = tiny_skia::Transform::from_translate(-(r.x as f32), -(r.y as f32));
        resvg::render(&tree, at, &mut pm.as_mut());
        Some((r, pm.take()))
    }

    /// Redraw `r` from every fiber over it, back to front, and hand it out
    /// straight-alpha.
    fn composite(&mut self, r: Rect) -> Patch {
        let w = self.w;
        for y in r.y..r.y + r.h {
            self.canvas[(y * w + r.x) * 4..(y * w + r.x + r.w) * 4].fill(0);
        }
        for f in &self.fibers {
            let Some(fr) = f.rect else { continue };
            let Some(i) = fr.intersect(r) else { continue };
            for y in i.y..i.y + i.h {
                let src = &f.pixels[((y - fr.y) * fr.w + i.x - fr.x) * 4..][..i.w * 4];
                let dst = &mut self.canvas[(y * w + i.x) * 4..][..i.w * 4];
                for (d, s) in dst.as_chunks_mut::<4>().0.iter_mut().zip(src.as_chunks::<4>().0) {
                    let keep = 255 - s[3] as u32;
                    for c in 0..4 {
                        d[c] = (s[c] as u32 + (d[c] as u32 * keep + 127) / 255) as u8;
                    }
                }
            }
        }
        let mut rgba = Vec::with_capacity(r.w * r.h * 4);
        for y in r.y..r.y + r.h {
            for s in self.canvas[(y * w + r.x) * 4..][..r.w * 4].as_chunks::<4>().0 {
                let a = s[3] as u32;
                let c = |v: u8| (v as u32 * 255).checked_div(a).unwrap_or(0).min(255) as u8;
                rgba.extend([c(s[0]), c(s[1]), c(s[2]), s[3]]);
            }
        }
        Patch { rect: r, rgba }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: usize = 64;

    /// A root and the buffer its patches paste into.
    struct Screen(Root, Vec<u8>);

    impl Screen {
        fn render(&mut self, elements: Vec<Element>) -> Vec<Rect> {
            let patches = self.0.render(elements);
            patches.iter().for_each(|p| p.paste(&mut self.1, W));
            patches.iter().map(|p| p.rect).collect()
        }
    }

    fn root() -> Screen {
        Screen(Root::new(W, 32, "", Arc::new(fontdb::Database::new())), vec![0; W * 32 * 4])
    }

    fn px(s: &Screen, x: usize, y: usize) -> [u8; 4] {
        s.1[(y * W + x) * 4..][..4].try_into().unwrap()
    }

    const RED: &str = r##"<rect x="0" y="0" width="8" height="8" fill="#ff0000"/>"##;

    #[test]
    fn an_unchanged_element_bails_out() {
        let mut r = root();
        assert_eq!(r.render(vec![svg("a", RED)]), vec![Rect { x: 0, y: 0, w: 8, h: 8 }]);
        assert_eq!(px(&r, 3, 3), [255, 0, 0, 255]);
        assert!(r.render(vec![svg("a", RED)]).is_empty());
    }

    #[test]
    fn an_unmounted_element_clears_its_rect() {
        let mut r = root();
        r.render(vec![svg("a", RED)]);
        assert_eq!(r.render(vec![]).len(), 1);
        assert_eq!(px(&r, 3, 3), [0, 0, 0, 0]);
    }

    #[test]
    fn later_elements_draw_over_earlier_ones() {
        let mut r = root();
        let rgb = [0, 0, 255].repeat(16 * 16);
        let half =
            r##"<rect x="0" y="0" width="16" height="16" fill="#ff0000" fill-opacity="0.5"/>"##;
        r.render(vec![image("img", Rect { x: 0, y: 0, w: 16, h: 16 }, &rgb), svg("wash", half)]);
        let [red, _, blue, a] = px(&r, 4, 4);
        assert!((120..136).contains(&red) && (120..136).contains(&blue) && a == 255);
        // A new frame under the wash recomposites the wash too.
        let green = [0, 255, 0].repeat(16 * 16);
        r.render(vec![image("img", Rect { x: 0, y: 0, w: 16, h: 16 }, &green), svg("wash", half)]);
        let [red, g, blue, _] = px(&r, 4, 4);
        assert!((120..136).contains(&red) && (120..136).contains(&g) && blue == 0);
    }

    #[test]
    fn an_image_past_the_edge_is_clipped_to_the_root() {
        let mut r = root();
        let rgb = [0, 255, 0].repeat(16 * 16);
        let at = Rect { x: W - 8, y: 24, w: 16, h: 16 };
        assert_eq!(
            r.render(vec![image("img", at, &rgb)]),
            vec![Rect { x: W - 8, y: 24, w: 8, h: 8 }]
        );
        assert_eq!(px(&r, W - 1, 31), [0, 255, 0, 255]);
        assert!(
            r.render(vec![image("img", at, &rgb)]).is_empty(),
            "a clipped image still bails out"
        );
    }

    const BLUE: &str = r##"<rect x="16" y="0" width="8" height="8" fill="#0000ff"/>"##;
    const GREEN: &str = r##"<rect x="32" y="0" width="8" height="8" fill="#00ff00"/>"##;

    #[test]
    fn an_element_toggled_before_others_leaves_them_alone() {
        let mut r = root();
        r.render(vec![svg("b", BLUE), svg("g", GREEN)]);
        assert_eq!(
            r.render(vec![svg("a", RED), svg("b", BLUE), svg("g", GREEN)]),
            vec![Rect { x: 0, y: 0, w: 8, h: 8 }]
        );
        assert_eq!(r.render(vec![svg("b", BLUE), svg("g", GREEN)]).len(), 1);
    }

    #[test]
    fn a_reorder_redraws_what_moved() {
        let mut r = root();
        r.render(vec![svg("b", BLUE), svg("g", GREEN)]);
        assert_eq!(r.render(vec![svg("g", GREEN), svg("b", BLUE)]).len(), 2);
    }

    #[test]
    fn a_repeated_key_keeps_the_first_and_leaves_no_stale_pixels() {
        let mut r = root();
        r.render(vec![svg("a", RED), svg("a", BLUE)]);
        assert_eq!(px(&r, 3, 3), [255, 0, 0, 255]);
        assert_eq!(px(&r, 19, 3), [0, 0, 0, 0], "the second one is dropped");
        r.render(vec![]);
        assert_eq!(px(&r, 3, 3), [0, 0, 0, 0]);
    }

    #[test]
    fn a_rect_dirtied_twice_is_patched_once() {
        let mut r = root();
        r.render(vec![svg("a", RED), svg("b", BLUE), svg("g", GREEN)]);
        // `x` mounts on `a`'s rect, `g` and `b` swap, then `a` unmounts.
        let patched = r.render(vec![svg("x", RED), svg("g", GREEN), svg("b", BLUE)]);
        assert_eq!(patched.len(), 3);
    }

    #[test]
    fn an_image_of_the_wrong_size_draws_nothing() {
        let mut r = root();
        let short = [0, 255, 0].repeat(15);
        assert!(r.render(vec![image("img", Rect { x: 0, y: 0, w: 4, h: 4 }, &short)]).is_empty());
        let outside = [0, 255, 0].repeat(4);
        assert!(r.render(vec![image("img", Rect { x: W, y: 0, w: 2, h: 2 }, &outside)]).is_empty());
        assert_eq!(px(&r, 0, 0), [0, 0, 0, 0]);
    }
}
