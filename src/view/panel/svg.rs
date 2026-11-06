//! The panel's fixed parts, from `assets/ui/panel.svg`: what every fragment
//! shares (icons, text style), the parts placed by id, and the faces.

use std::sync::{Arc, OnceLock};

use reactive_svg::fontdb;

const TEMPLATE: &str = include_str!("../../../assets/ui/panel.svg");

/// The `<defs>` and `<style>` every fragment is parsed with.
pub fn head() -> &'static str {
    let start = TEMPLATE.find("<defs>").expect("panel.svg has <defs>");
    let end = TEMPLATE.find("</style>").expect("panel.svg has <style>") + "</style>".len();
    &TEMPLATE[start..end]
}

/// The body of `<g id="{id}">`. Layout groups do not nest, so it ends at the
/// first `</g>`.
pub fn layout(id: &str) -> &'static str {
    let open = format!(r#"<g id="{id}">"#);
    let start = TEMPLATE.find(&open).unwrap_or_else(|| panic!("panel.svg has no {open}"));
    let body = &TEMPLATE[start + open.len()..];
    &body[..body.find("</g>").expect("an open <g> closes")]
}

pub fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

pub fn fonts() -> &'static Arc<fontdb::Database> {
    static FONTS: OnceLock<Arc<fontdb::Database>> = OnceLock::new();
    FONTS.get_or_init(|| {
        let mut db = fontdb::Database::new();
        db.load_font_data(include_bytes!("../../../assets/fonts/NotoSansMono-ascii.ttf").to_vec());
        db.load_font_data(include_bytes!("../../../assets/fonts/NotoSansCJKjp-cp932.otf").to_vec());
        db.set_sans_serif_family("Noto Sans Mono");
        db.set_serif_family("Noto Sans Mono");
        db.set_monospace_family("Noto Sans Mono");
        Arc::new(db)
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn every_part_the_panel_places_is_in_the_template() {
        let ids = ["band", "rec", "refusal", "centre_back", "rec_failed"];
        let cells = (0..3).flat_map(|n| ["bars", "nothumb", "wash"].map(|p| format!("c{n}_{p}")));
        for id in ids.into_iter().map(String::from).chain(cells) {
            assert!(!super::layout(&id).trim().is_empty(), "{id}");
        }
    }
}
