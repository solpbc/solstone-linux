// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use gtk::cairo;
use gtk::prelude::*;
use resvg::usvg::{Options, Tree, fontdb};
use solstone_linux::copy::CopyKey;
use solstone_linux::window_control::MarkRenderDocument;
use std::sync::Arc;

static FONT_DATA: &[u8] = include_bytes!("../assets/Comfortaa-Bold.ttf");

pub fn mark_card_accessible_name(mark: &MarkRenderDocument) -> String {
    solstone_linux::copy::journal_mark_speech(
        &mark.icon1.color.name,
        &mark.icon2.color.name,
        &mark.words[0],
        &mark.words[1],
    )
}

pub fn create_font_database() -> fontdb::Database {
    let mut db = fontdb::Database::new();
    db.load_font_data(FONT_DATA.to_vec());
    db
}

const CARD_WIDTH: f32 = 240.0;
const CARD_HEIGHT: f32 = 130.0;
const CARD_SIDE_PADDING: f32 = 22.0;

/// The width of the two words as the card draws them, in the bundled font.
fn measured_words_width(word1: &str, word2: &str) -> Option<f32> {
    let svg = format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="1" height="1"><text font-family="Comfortaa" font-weight="700" font-size="18" xml:space="preserve">{word1} · {word2}</text></svg>"##
    );
    let options = Options {
        fontdb: Arc::new(create_font_database()),
        ..Options::default()
    };
    let tree = Tree::from_str(&svg, &options).ok()?;
    Some(tree.root().abs_bounding_box().width())
}

/// The card hugs its content, and never gets narrower than the chips' usual card.
pub fn mark_card_width(mark: &MarkRenderDocument) -> f32 {
    let word1 = gtk::glib::markup_escape_text(&mark.words[0].to_lowercase());
    let word2 = gtk::glib::markup_escape_text(&mark.words[1].to_lowercase());
    measured_words_width(&word1, &word2).map_or(CARD_WIDTH, |words| {
        (words + 2.0 * CARD_SIDE_PADDING).ceil().max(CARD_WIDTH)
    })
}

pub fn build_mark_card_svg(mark: &MarkRenderDocument) -> String {
    let total_width = mark_card_width(mark);
    let total_height = CARD_HEIGHT;
    let chip_y = 20.0;
    let chip1_cx = (total_width - (48.0 * 2.0 + 11.0)) / 2.0 + 24.0;
    let chip1_cy = chip_y + 24.0;
    let chip2_cx = chip1_cx + 48.0 + 11.0;
    let chip2_cy = chip_y + 24.0;

    let w1 = gtk::glib::markup_escape_text(&mark.words[0].to_lowercase());
    let w2 = gtk::glib::markup_escape_text(&mark.words[1].to_lowercase());

    format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="{total_width}" height="{total_height}" viewBox="0 0 {total_width} {total_height}">
  <rect x="0.5" y="0.5" width="{}" height="{}" rx="12" fill="#fffdf9" stroke="#e7d8c6" stroke-width="1"/>
  <g transform="rotate({}, {chip1_cx}, {chip1_cy})">
    <rect x="{}" y="{}" width="48" height="48" rx="12" fill="{}" fill-opacity="0.12" stroke="{}" stroke-width="2"/>
    <g transform="translate({}, {})">
      <svg width="28" height="28" viewBox="0 0 24 24" fill="none" stroke="{}" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
        {}
      </svg>
    </g>
  </g>
  <g transform="rotate({}, {chip2_cx}, {chip2_cy})">
    <rect x="{}" y="{}" width="48" height="48" rx="12" fill="{}" fill-opacity="0.12" stroke="{}" stroke-width="2"/>
    <g transform="translate({}, {})">
      <svg width="28" height="28" viewBox="0 0 24 24" fill="none" stroke="{}" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
        {}
      </svg>
    </g>
  </g>
  <text x="{}" y="105" font-family="Comfortaa" font-weight="700" font-size="18" text-anchor="middle" xml:space="preserve"><tspan fill="#1A1A1A">{w1}</tspan><tspan fill="#6E6453"> · </tspan><tspan fill="#1A1A1A">{w2}</tspan></text>
</svg>"##,
        total_width - 1.0,
        total_height - 1.0,
        mark.icon1.rot,
        chip1_cx - 24.0,
        chip1_cy - 24.0,
        mark.icon1.color.hex,
        mark.icon1.color.hex,
        chip1_cx - 14.0,
        chip1_cy - 14.0,
        mark.icon1.color.hex,
        mark.icon1.svg,
        mark.icon2.rot,
        chip2_cx - 24.0,
        chip2_cy - 24.0,
        mark.icon2.color.hex,
        mark.icon2.color.hex,
        chip2_cx - 14.0,
        chip2_cy - 14.0,
        mark.icon2.color.hex,
        mark.icon2.svg,
        total_width / 2.0,
    )
}

pub fn render_mark_card_svg(svg_str: &str, scale: f32) -> Option<resvg::tiny_skia::Pixmap> {
    let fontdb = create_font_database();
    let opt = Options {
        fontdb: Arc::new(fontdb),
        ..Options::default()
    };
    let tree = Tree::from_str(svg_str, &opt).ok()?;
    let width = (tree.size().width() * scale).ceil() as u32;
    let height = (tree.size().height() * scale).ceil() as u32;
    let mut pixmap = resvg::tiny_skia::Pixmap::new(width, height)?;
    let transform = resvg::tiny_skia::Transform::from_scale(scale, scale);
    resvg::render(&tree, transform, &mut pixmap.as_mut());
    Some(pixmap)
}

pub fn semibold_label(text: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    let attributes = gtk::pango::AttrList::new();
    attributes.insert(gtk::pango::AttrInt::new_weight(
        gtk::pango::Weight::Semibold,
    ));
    label.set_attributes(Some(&attributes));
    label
}

pub fn create_mark_card_widget(mark: MarkRenderDocument) -> gtk::Widget {
    let accessible_name = mark_card_accessible_name(&mark);
    let width = mark_card_width(&mark);
    create_card_widget(build_mark_card_svg(&mark), accessible_name, width)
}

/// The card with its heading above it, for a mark the owner has confirmed.
pub fn create_headed_mark_card_widget(mark: MarkRenderDocument, heading: CopyKey) -> gtk::Widget {
    let card = gtk::Box::new(gtk::Orientation::Vertical, 8);
    card.set_halign(gtk::Align::Center);
    card.append(&semibold_label(heading.as_str()));
    card.append(&create_mark_card_widget(mark));
    card.upcast()
}

/// What a mark slot draws when it has no mark.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EmptyMark {
    /// No journal identity here yet.
    Generic,
    /// A mark exists and can't be shown right now.
    Unavailable,
    /// Something answered and presented no identity. It isn't ours, so no brand colour.
    NotPresented,
}

fn empty_mark_card_svg(state: EmptyMark) -> (String, &'static str) {
    let (first, second, speech, fill1, fill2, border1, border2, alpha, dash, rotation, glyph) =
        match state {
            EmptyMark::Unavailable => (
                CopyKey::MarkUnavailableFirst,
                CopyKey::MarkUnavailableSecond,
                CopyKey::MarkUnavailableSpeech,
                "#FEFCF8",
                "#FEFCF8",
                "#ECE3D0",
                "#ECE3D0",
                "1",
                "none",
                0,
                "?",
            ),
            EmptyMark::Generic => (
                CopyKey::MarkGenericFirst,
                CopyKey::MarkGenericSecond,
                CopyKey::MarkGenericSpeech,
                "#E8913A",
                "#D4A017",
                "#E8913A",
                "#D4A017",
                "0.07",
                "5.69 4.27",
                45,
                "",
            ),
            EmptyMark::NotPresented => (
                CopyKey::MarkNotPresentedFirst,
                CopyKey::MarkNotPresentedSecond,
                CopyKey::UnknownJournalNoIdentity,
                "#6E6453",
                "#6E6453",
                "#6E6453",
                "#6E6453",
                "0.07",
                "5.69 4.27",
                45,
                "",
            ),
        };
    let w1 = gtk::glib::markup_escape_text(first.as_str());
    let w2 = gtk::glib::markup_escape_text(second.as_str());
    let svg = format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="240" height="130" viewBox="0 0 240 130">
<rect x="0.5" y="0.5" width="239" height="129" rx="12" fill="#fffdf9" stroke="#e7d8c6"/>
<rect x="66.5" y="20" width="48" height="48" rx="12" fill="{fill1}" fill-opacity="{alpha}" stroke="{border1}" stroke-width="2" stroke-dasharray="{dash}"/>
<g transform="rotate({rotation},149.5,44)"><rect x="125.5" y="20" width="48" height="48" rx="12" fill="{fill2}" fill-opacity="{alpha}" stroke="{border2}" stroke-width="2" stroke-dasharray="{dash}"/></g>
<text x="90.5" y="53" font-family="Comfortaa" font-size="28" text-anchor="middle" fill="#6E6453">{glyph}</text>
<text x="149.5" y="53" font-family="Comfortaa" font-size="28" text-anchor="middle" fill="#6E6453">{glyph}</text>
<text x="120" y="105" font-family="Comfortaa" font-weight="700" font-size="18" text-anchor="middle" xml:space="preserve"><tspan fill="#1A1A1A">{w1}</tspan><tspan fill="#6E6453"> · </tspan><tspan fill="#1A1A1A">{w2}</tspan></text></svg>"##
    );
    (svg, speech.as_str())
}

pub fn create_empty_mark_widget(unavailable: bool) -> gtk::Widget {
    create_empty_state_widget(if unavailable {
        EmptyMark::Unavailable
    } else {
        EmptyMark::Generic
    })
}

/// The "what answered" slot when nothing was claimed.
pub fn create_not_presented_mark_widget() -> gtk::Widget {
    create_empty_state_widget(EmptyMark::NotPresented)
}

fn create_empty_state_widget(state: EmptyMark) -> gtk::Widget {
    let (svg, speech) = empty_mark_card_svg(state);
    create_card_widget(svg, speech.into(), CARD_WIDTH)
}

fn create_card_widget(svg: String, accessible_name: String, width: f32) -> gtk::Widget {
    // GtkBuilder is how a GTK 4.6 binding sets an accessible label: the direct call
    // needs the crate's 4.10 feature, above this window's floor.
    let name = gtk::glib::markup_escape_text(&accessible_name);
    let ui = format!(
        r#"<interface><object class="GtkDrawingArea" id="card"><property name="accessible-role">img</property><property name="focusable">true</property><accessibility><property name="label">{name}</property></accessibility></object></interface>"#
    );
    let area: gtk::DrawingArea = gtk::Builder::from_string(&ui)
        .object("card")
        .expect("the card's interface names its one object");
    area.set_content_width(width as i32);
    area.set_content_height(CARD_HEIGHT as i32);

    area.set_draw_func(move |widget, cr, _width, _height| {
        let scale = widget.scale_factor() as f32;
        if let Some(pixmap) = render_mark_card_svg(&svg, scale) {
            let width = pixmap.width() as i32;
            let height = pixmap.height() as i32;
            let stride = width * 4;
            let mut cairo_data = pixmap.data().to_vec();
            for pixel in cairo_data.chunks_exact_mut(4) {
                let r = pixel[0];
                let g = pixel[1];
                let b = pixel[2];
                let a = pixel[3];
                pixel[0] = b;
                pixel[1] = g;
                pixel[2] = r;
                pixel[3] = a;
            }
            if let Ok(surface) = cairo::ImageSurface::create_for_data(
                cairo_data,
                cairo::Format::ARgb32,
                width,
                height,
                stride,
            ) {
                cr.save().ok();
                cr.scale(1.0 / scale as f64, 1.0 / scale as f64);
                let _ = cr.set_source_surface(&surface, 0.0, 0.0);
                let _ = cr.paint();
                cr.restore().ok();
            }
        }
    });

    area.set_halign(gtk::Align::Center);
    area.upcast()
}

#[cfg(test)]
mod tests {
    use super::*;
    use solstone_linux::window_control::{MarkColorDocument, MarkIconDocument, MarkRenderDocument};

    fn dark_pixels(
        pixmap: &resvg::tiny_skia::Pixmap,
        xs: std::ops::Range<u32>,
        ys: std::ops::Range<u32>,
    ) -> usize {
        ys.flat_map(|y| xs.clone().map(move |x| (x, y)))
            .filter(|&(x, y)| {
                let pixel = pixmap.pixel(x, y).unwrap();
                pixel.alpha() > 200
                    && pixel.red() < 140
                    && pixel.green() < 140
                    && pixel.blue() < 140
            })
            .count()
    }

    #[test]
    fn empty_mark_words_and_unavailable_questions_draw_visible_ink() {
        for state in [
            EmptyMark::Generic,
            EmptyMark::Unavailable,
            EmptyMark::NotPresented,
        ] {
            let (svg, _) = empty_mark_card_svg(state);
            let pixmap = render_mark_card_svg(&svg, 1.0).unwrap();
            assert!(
                dark_pixels(&pixmap, 20..220, 85..115) > 50,
                "empty mark words must remain visible"
            );
            if state == EmptyMark::Unavailable {
                assert!(
                    dark_pixels(&pixmap, 75..105, 25..60) > 10,
                    "first unavailable chip must contain its question mark"
                );
                assert!(
                    dark_pixels(&pixmap, 135..165, 25..60) > 10,
                    "second unavailable chip must contain its question mark"
                );
            }
        }
    }

    #[test]
    fn nothing_claimed_keeps_the_slot_and_speaks_its_own_name() {
        let (not_presented, speech) = empty_mark_card_svg(EmptyMark::NotPresented);
        let (generic, generic_speech) = empty_mark_card_svg(EmptyMark::Generic);
        assert_eq!(speech, CopyKey::UnknownJournalNoIdentity.as_str());
        assert_ne!(speech, generic_speech);

        // Same two-word slot, so the layout doesn't jump, in its own colour.
        let not_presented = render_mark_card_svg(&not_presented, 1.0).unwrap();
        let generic = render_mark_card_svg(&generic, 1.0).unwrap();
        assert_eq!(not_presented.width(), generic.width());
        assert_eq!(not_presented.height(), generic.height());
        assert_ne!(not_presented.data(), generic.data());
    }

    fn mark_with_words(word1: &str, word2: &str) -> MarkRenderDocument {
        let icon = |name: &str, rot| MarkIconDocument {
            name: name.into(),
            svg: "<path d=\"M0 0 L10 10\"/>".into(),
            color: MarkColorDocument {
                name: "gold".into(),
                hex: "#ffd700".into(),
            },
            rot,
        };
        MarkRenderDocument {
            icon1: icon("star", 0),
            icon2: icon("moon", 45),
            words: [word1.into(), word2.into()],
        }
    }

    #[test]
    fn the_longest_words_stay_inside_the_card() {
        let short = mark_with_words("afoot", "unfixed");
        assert_eq!(mark_card_width(&short), CARD_WIDTH);

        let long = mark_with_words("mammogram", "womanhood");
        let width = mark_card_width(&long);
        assert!(width > CARD_WIDTH, "got {width}");
        let words = measured_words_width("mammogram", "womanhood").unwrap();
        assert!(
            width - words >= 2.0 * CARD_SIDE_PADDING,
            "got {width}, {words}"
        );

        // Rendered, the words leave the card's own edge columns clear.
        let pixmap = render_mark_card_svg(&build_mark_card_svg(&long), 1.0).unwrap();
        assert_eq!(pixmap.width(), width.ceil() as u32);
        assert_eq!(dark_pixels(&pixmap, 2..10, 85..115), 0);
        assert_eq!(
            dark_pixels(&pixmap, pixmap.width() - 10..pixmap.width() - 2, 85..115),
            0
        );
        assert!(dark_pixels(&pixmap, 10..pixmap.width() - 10, 85..115) > 50);
    }

    #[test]
    fn accessible_name_matches_speech() {
        let mark = MarkRenderDocument {
            icon1: MarkIconDocument {
                name: "star".into(),
                svg: "<path d=\"M0 0 L10 10\"/>".into(),
                color: MarkColorDocument {
                    name: "gold".into(),
                    hex: "#ffd700".into(),
                },
                rot: 0,
            },
            icon2: MarkIconDocument {
                name: "moon".into(),
                svg: "<path d=\"M0 0 L10 10\"/>".into(),
                color: MarkColorDocument {
                    name: "silver".into(),
                    hex: "#c0c0c0".into(),
                },
                rot: 45,
            },
            words: ["amber".into(), "river".into()],
        };
        let expected =
            solstone_linux::copy::journal_mark_speech("gold", "silver", "amber", "river");
        assert_eq!(mark_card_accessible_name(&mark), expected);

        let svg = build_mark_card_svg(&mark);
        assert!(!svg.is_empty());
        let database = create_font_database();
        let faces: Vec<_> = database
            .faces()
            .map(|face| (&face.families, face.weight))
            .collect();
        assert!(
            database
                .query(&fontdb::Query {
                    families: &[fontdb::Family::Name("Comfortaa")],
                    weight: fontdb::Weight::BOLD,
                    ..Default::default()
                })
                .is_some(),
            "bundled Comfortaa must resolve: {faces:?}"
        );
        let pixmap = render_mark_card_svg(&svg, 1.0).unwrap();
        let dark_word_pixels = dark_pixels(&pixmap, 20..220, 85..115);
        assert!(
            dark_word_pixels > 50,
            "mark words must draw visible ink; got {dark_word_pixels}, fonts {faces:?}"
        );
    }
}
