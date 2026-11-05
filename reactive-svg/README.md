# reactive-svg

Draws a screen of keyed SVG fragments into one RGBA texture and returns only
the rectangles that changed. YUBI Go uses it for the headset panel: a panel
thread renders every 100 ms, and the render thread uploads the patches into an
OpenXR swapchain.

Each frame, the caller passes `Root::render` the whole list of elements. An
element is either a fragment of SVG in the root's pixel coordinates (`svg`) or
a packed RGB image (`image`). An element that is equal to the one rendered
under the same key last frame is not rasterized again. Elements composite in
list order, so later ones draw over earlier ones. The dirty rectangle of each
element is the bounding box that resvg reports, so the caller never writes one
by hand.

```rust
use std::sync::Arc;
use reactive_svg::{fontdb, svg, Root};

let mut root = Root::new(256, 64, "", Arc::new(fontdb::Database::new()));
let red = r##"<rect x="0" y="0" width="8" height="8" fill="#ff0000"/>"##;
let patches = root.render(vec![svg("square", red)]);
assert_eq!(patches.len(), 1);
assert!(root.render(vec![svg("square", red)]).is_empty());
```

Each `Patch` is straight-alpha RGBA, rows top-down. `Patch::paste` copies it
into a buffer the size of the root.

`signal` is a separate helper: a setter and a getter across threads, where the
getter reads the last value set. The panel uses it to pass controller state to
its thread.

## Limits

- Keys must be unique within one frame. A repeated key leaves stale pixels.
- Fonts come from the caller's `fontdb::Database`. System fonts and `<image>`
  decoding are compiled out.
