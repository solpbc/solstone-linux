// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! The window's ground: a header bar painted the sun-arc ground, the desktop's own
//! buttons, and the sun-arc frame in the content. The shelf is an inset card. There
//! is no backdrop blur.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use gtk::cairo::{self, ImageSurface};
use gtk::gio;
use gtk::glib;
use gtk::prelude::*;
use solstone_linux::sun_arc::{self, Appearance, Glow, LocalSun, Rgb, SunTimes};

const MARK_SVG: &str = include_str!("../assets/mark.svg");
const GLOW_MID_STOP: f64 = 0.38;
const GLOW_MID_RATIO: f64 = 0.45;
const SHELF_WIDTH: i32 = 186;

struct Paint {
    css: gtk::CssProvider,
    css_on_display: Cell<bool>,
    last_css: RefCell<String>,
    mark: RefCell<Option<(i32, Rc<ImageSurface>)>>,
    times: RefCell<Option<(String, SunTimes)>>,
    scheme: Cell<Option<Appearance>>,
    scheme_at: Cell<Option<Instant>>,
    portal: RefCell<Option<gio::DBusProxy>>,
    portal_miss_at: Cell<Option<Instant>>,
    minute_tick: RefCell<Option<glib::SourceId>>,
    scheme_tick: RefCell<Option<glib::SourceId>>,
    frame_shot: RefCell<Option<glib::SourceId>>,
    toplevel_hooked: Cell<bool>,
}

/// Header bar, sun-arc content, and the inset shelf. The bar shows no title text.
/// The window title stays `solstone`, which is the accessible name.
pub fn attach(window: &gtk::ApplicationWindow) -> gtk::Widget {
    if frame_wanted() {
        eprintln!("frame-file attach");
    }
    window.set_title(Some(solstone_linux::copy::CopyKey::WindowTitle.as_str()));

    let header = gtk::HeaderBar::new();
    header.set_show_title_buttons(true);
    header.set_title_widget(Some(&gtk::Box::new(gtk::Orientation::Horizontal, 0)));
    window.set_titlebar(Some(&header));

    let paint = Rc::new(Paint {
        css: gtk::CssProvider::new(),
        css_on_display: Cell::new(false),
        last_css: RefCell::new(String::new()),
        mark: RefCell::new(None),
        times: RefCell::new(None),
        scheme: Cell::new(None),
        scheme_at: Cell::new(None),
        portal: RefCell::new(None),
        portal_miss_at: Cell::new(None),
        minute_tick: RefCell::new(None),
        scheme_tick: RefCell::new(None),
        frame_shot: RefCell::new(None),
        toplevel_hooked: Cell::new(false),
    });
    apply_css(
        &paint,
        &Rgb {
            r: 0xFE,
            g: 0xFC,
            b: 0xF8,
        }
        .css(),
    );

    let root = gtk::Overlay::new();
    root.set_hexpand(true);
    root.set_vexpand(true);

    let canvas = gtk::DrawingArea::new();
    canvas.set_hexpand(true);
    canvas.set_vexpand(true);
    canvas.set_can_focus(false);
    root.set_child(Some(&canvas));

    let shelf = gtk::Box::new(gtk::Orientation::Vertical, 0);
    shelf.add_css_class("sol-shelf");
    shelf.set_size_request(SHELF_WIDTH, -1);
    shelf.set_halign(gtk::Align::Start);
    shelf.set_valign(gtk::Align::Fill);
    shelf.set_vexpand(true);
    shelf.set_margin_start(8);
    shelf.set_margin_top(8);
    shelf.set_margin_bottom(8);
    shelf.set_can_focus(false);
    root.add_overlay(&shelf);

    let paint_draw = Rc::clone(&paint);
    let shelf_draw = shelf.clone();
    let window_draw = window.clone();
    canvas.set_draw_func(move |_area, cr, width, height| {
        paint_frame(&paint_draw, &window_draw, &shelf_draw, cr, width, height);
    });

    let paint_map = Rc::clone(&paint);
    let canvas_map = canvas.clone();
    window.connect_map(move |win| {
        ensure_css_provider(win, &paint_map);
        hook_toplevel(win, &paint_map, &canvas_map);
        sync_ticks(win, &paint_map, &canvas_map);
        canvas_map.queue_draw();
    });

    let paint_unmap = Rc::clone(&paint);
    let canvas_unmap = canvas.clone();
    window.connect_unmap(move |win| {
        sync_ticks(win, &paint_unmap, &canvas_unmap);
    });

    let paint_dark = Rc::clone(&paint);
    let canvas_dark = canvas.clone();
    if let Some(settings) = gtk::Settings::default() {
        settings.connect_gtk_application_prefer_dark_theme_notify(move |_| {
            paint_dark.scheme_at.set(None);
            canvas_dark.queue_draw();
        });
    }

    let paint_destroy = Rc::clone(&paint);
    window.connect_destroy(move |_| {
        stop_tick(&paint_destroy.minute_tick);
        stop_tick(&paint_destroy.scheme_tick);
        stop_tick(&paint_destroy.frame_shot);
    });

    queue_frame_file(window, &paint);

    root.upcast()
}

/// Writes one PNG of this window when `SOLSTONE_FRAME_PNG` is set.
/// The timer stops once the file is written, or after a few tries.
fn frame_wanted() -> bool {
    std::env::var("SOLSTONE_FRAME_PNG").is_ok_and(|path| !path.is_empty())
}

fn queue_frame_file(window: &gtk::ApplicationWindow, paint: &Rc<Paint>) {
    let Ok(path) = std::env::var("SOLSTONE_FRAME_PNG") else {
        return;
    };
    if path.is_empty() {
        return;
    }
    eprintln!("frame-file scheduled");
    let window = window.clone();
    let paint_tick = Rc::clone(paint);
    let tries = Cell::new(0u8);
    let id = glib::timeout_add_local(Duration::from_millis(400), move || {
        let attempt = tries.get().saturating_add(1);
        tries.set(attempt);
        let written = write_frame_file(&window, &path);
        if written || attempt >= 12 {
            // Break stops the source. Take the id so destroy does not call remove()
            // on a source that is already gone: that call panics. SourceId's Drop
            // does not remove it.
            let _gone = paint_tick.frame_shot.borrow_mut().take();
            glib::ControlFlow::Break
        } else {
            glib::ControlFlow::Continue
        }
    });
    *paint.frame_shot.borrow_mut() = Some(id);
}

pub(crate) fn try_frame_file(window: &gtk::ApplicationWindow) -> bool {
    let Ok(path) = std::env::var("SOLSTONE_FRAME_PNG") else {
        return false;
    };
    if path.is_empty() {
        return false;
    }
    write_frame_file(window, &path)
}

fn write_frame_file(window: &gtk::ApplicationWindow, path: &str) -> bool {
    let width = window.width();
    let height = window.height();
    if !window.is_mapped() || width < 400 || height < 400 {
        frame_note(
            path,
            &format!("wait mapped={} {width}x{height}\n", window.is_mapped()),
        );
        return false;
    }
    let Some(renderer) = window.native().and_then(|native| native.renderer()) else {
        frame_note(path, "no-renderer\n");
        return false;
    };
    let save = |widget: &gtk::Widget, shot_width: i32, shot_height: i32, dest: &str| -> bool {
        if shot_width < 2 || shot_height < 2 {
            return false;
        }
        let paintable = gtk::WidgetPaintable::new(Some(widget));
        let snapshot = gtk::Snapshot::new();
        paintable.snapshot(&snapshot, f64::from(shot_width), f64::from(shot_height));
        let Some(node) = snapshot.to_node() else {
            return false;
        };
        let texture = renderer.render_texture(&node, None);
        texture.save_to_png(dest).is_ok()
    };
    if save(window.upcast_ref(), width, height, path) {
        frame_note(path, &format!("ok {width}x{height}\n"));
        return true;
    }
    // A window's own snapshot can be empty. The bar and the child still draw.
    let header_saved = window.titlebar().is_some_and(|bar| {
        let bar_height = bar.height();
        save(
            bar.upcast_ref(),
            width,
            bar_height,
            &format!("{path}.header.png"),
        )
    });
    let child_saved = window.child().is_some_and(|child| {
        let child_height = child.height();
        save(
            child.upcast_ref(),
            width,
            child_height,
            &format!("{path}.child.png"),
        )
    });
    frame_note(
        path,
        &format!("parts header={header_saved} child={child_saved}\n"),
    );
    header_saved && child_saved
}

fn frame_note(path: &str, line: &str) {
    eprint!("{line}");
    let _ = std::fs::write(format!("{path}.txt"), line);
}

fn paint_frame(
    paint: &Paint,
    window: &gtk::ApplicationWindow,
    shelf: &gtk::Box,
    cr: &cairo::Context,
    width: i32,
    height: i32,
) {
    ensure_css_provider(window, paint);
    if width < 2 || height < 2 {
        return;
    }
    let appearance = current_appearance(paint);
    if appearance == Appearance::Dark {
        shelf.add_css_class("sol-shelf-dark");
    } else {
        shelf.remove_css_class("sol-shelf-dark");
    }
    let sun = sun_arc::local_sun();
    let times = cached_times(paint, &sun);
    let frame = sun_arc::frame(
        f64::from(width),
        f64::from(height),
        sun.minutes,
        times.rise,
        times.set,
        appearance,
    );
    apply_css(paint, &frame.ground.css());

    cr.rectangle(0.0, 0.0, f64::from(width), f64::from(height));
    cr.clip();
    paint_rgb(cr, frame.ground);
    // A day frame draws no night sky. The still night field is painted only once
    // `frame.night` is above zero, and that path is not this draw.
    if let Some(glow) = frame.glow {
        paint_glow(cr, glow);
    }
    if let Some(halo) = frame.halo {
        paint_glow(cr, halo);
    }
    if frame.on && frame.opacity > 0.001 {
        paint_mark(paint, cr, &frame);
    }
}

fn cached_times(paint: &Paint, sun: &LocalSun) -> SunTimes {
    let key = format!("{}|{}|{:.3}", sun.zone_id, sun.date, sun.offset_minutes);
    if let Some((cached_key, times)) = paint.times.borrow().as_ref()
        && cached_key == &key
    {
        return *times;
    }
    *paint.times.borrow_mut() = Some((key, sun.times));
    sun.times
}

fn paint_rgb(cr: &cairo::Context, color: Rgb) {
    cr.set_source_rgb(
        f64::from(color.r) / 255.0,
        f64::from(color.g) / 255.0,
        f64::from(color.b) / 255.0,
    );
    let _ = cr.paint();
}

fn paint_glow(cr: &cairo::Context, glow: Glow) {
    if glow.a <= 0.002 || glow.r < 1.0 {
        return;
    }
    let (red, green, blue) = (
        f64::from(glow.color.r) / 255.0,
        f64::from(glow.color.g) / 255.0,
        f64::from(glow.color.b) / 255.0,
    );
    let pattern = cairo::RadialGradient::new(glow.x, glow.y, 0.0, glow.x, glow.y, glow.r);
    pattern.add_color_stop_rgba(0.0, red, green, blue, glow.a);
    pattern.add_color_stop_rgba(GLOW_MID_STOP, red, green, blue, glow.a * GLOW_MID_RATIO);
    pattern.add_color_stop_rgba(1.0, red, green, blue, 0.0);
    let _ = cr.set_source(&pattern);
    let _ = cr.paint();
}

fn paint_mark(paint: &Paint, cr: &cairo::Context, frame: &sun_arc::SunFrame) {
    let Some(diameter) = round_diameter(frame.geom.r) else {
        return;
    };
    let Some(surface) = mark_surface(paint, diameter) else {
        return;
    };
    let left = frame.geom.pos.x - f64::from(diameter) / 2.0;
    let top = frame.geom.pos.y - f64::from(diameter) / 2.0;
    if cr.set_source_surface(surface.as_ref(), left, top).is_err() {
        return;
    }
    let _ = cr.paint_with_alpha(frame.opacity);
}

fn round_diameter(radius: f64) -> Option<i32> {
    let diameter = (2.0 * radius).round();
    if !diameter.is_finite() || diameter < 2.0 || diameter > f64::from(i32::MAX) {
        return None;
    }
    #[allow(clippy::cast_possible_truncation)]
    Some(diameter as i32)
}

fn mark_surface(paint: &Paint, diameter: i32) -> Option<Rc<ImageSurface>> {
    if diameter < 2 {
        return None;
    }
    if let Some((cached, surface)) = paint.mark.borrow().as_ref()
        && *cached == diameter
    {
        return Some(Rc::clone(surface));
    }
    let surface = Rc::new(render_mark(diameter)?);
    *paint.mark.borrow_mut() = Some((diameter, Rc::clone(&surface)));
    Some(surface)
}

fn render_mark(diameter: i32) -> Option<ImageSurface> {
    let options = resvg::usvg::Options::default();
    let tree = resvg::usvg::Tree::from_str(MARK_SVG, &options).ok()?;
    let size = tree.size().width();
    if size <= 0.0 {
        return None;
    }
    let px = u32::try_from(diameter).ok()?;
    let scale = px as f32 / size;
    let mut pixmap = resvg::tiny_skia::Pixmap::new(px, px)?;
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    // tiny-skia is already premultiplied RGBA. Cairo ARgb32 is premultiplied,
    // little-endian B,G,R,A. Swap the channels and leave the alpha alone.
    let mut data = pixmap.data().to_vec();
    for pixel in data.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    let stride = diameter * 4;
    ImageSurface::create_for_data(data, cairo::Format::ARgb32, diameter, diameter, stride).ok()
}

fn apply_css(paint: &Paint, ground: &str) {
    let css = format!(
        "\
headerbar, headerbar:backdrop, .titlebar, window, window.background {{
  background-color: {ground};
  background-image: none;
  box-shadow: none;
  border-bottom-width: 0;
}}
.sol-shelf {{
  background-color: rgba(255, 252, 246, 0.55);
  border-radius: 16px;
  border: 1px solid rgba(255, 255, 255, 0.70);
  box-shadow: 0 6px 22px rgba(80, 50, 10, 0.14);
}}
.sol-shelf-dark {{
  background-color: rgba(42, 34, 28, 0.62);
  border-color: rgba(255, 244, 230, 0.18);
  box-shadow: 0 6px 22px rgba(0, 0, 0, 0.28);
}}
"
    );
    if paint.last_css.borrow().as_str() == css {
        return;
    }
    paint.css.load_from_data(&css);
    *paint.last_css.borrow_mut() = css;
}

fn ensure_css_provider(window: &gtk::ApplicationWindow, paint: &Paint) {
    if paint.css_on_display.get() {
        return;
    }
    let display = gtk::prelude::WidgetExt::display(window);
    gtk::style_context_add_provider_for_display(
        &display,
        &paint.css,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
    paint.css_on_display.set(true);
}

fn current_appearance(paint: &Paint) -> Appearance {
    if let Some(appearance) = paint.scheme.get()
        && paint
            .scheme_at
            .get()
            .is_some_and(|at| at.elapsed() < Duration::from_millis(200))
    {
        return appearance;
    }
    let appearance = read_appearance(paint);
    paint.scheme.set(Some(appearance));
    paint.scheme_at.set(Some(Instant::now()));
    appearance
}

/// Portal `color-scheme`: 1 dark, 2 light, 0 or no portal follows the GTK dark setting.
/// This reads the setting. It does not write the portal.
fn read_appearance(paint: &Paint) -> Appearance {
    let scheme = portal_proxy(paint).and_then(|proxy| portal_scheme(&proxy));
    let settings = gtk::Settings::default();
    match scheme {
        Some(1) => {
            set_prefer_dark(&settings, true);
            Appearance::Dark
        }
        Some(2) => {
            set_prefer_dark(&settings, false);
            Appearance::Light
        }
        _ => {
            if settings.is_some_and(|value| value.is_gtk_application_prefer_dark_theme()) {
                Appearance::Dark
            } else {
                Appearance::Light
            }
        }
    }
}

fn set_prefer_dark(settings: &Option<gtk::Settings>, dark: bool) {
    let Some(settings) = settings else {
        return;
    };
    if settings.is_gtk_application_prefer_dark_theme() != dark {
        settings.set_gtk_application_prefer_dark_theme(dark);
    }
}

fn portal_proxy(paint: &Paint) -> Option<gio::DBusProxy> {
    if let Some(proxy) = paint.portal.borrow().as_ref().cloned() {
        return Some(proxy);
    }
    if paint
        .portal_miss_at
        .get()
        .is_some_and(|at| at.elapsed() < Duration::from_secs(2))
    {
        return None;
    }
    match gio::DBusProxy::for_bus_sync(
        gio::BusType::Session,
        gio::DBusProxyFlags::NONE,
        None,
        "org.freedesktop.portal.Desktop",
        "/org/freedesktop/portal/desktop",
        "org.freedesktop.portal.Settings",
        None::<&gio::Cancellable>,
    ) {
        Ok(proxy) => {
            paint.portal_miss_at.set(None);
            *paint.portal.borrow_mut() = Some(proxy.clone());
            Some(proxy)
        }
        Err(_) => {
            paint.portal_miss_at.set(Some(Instant::now()));
            None
        }
    }
}

fn portal_scheme(proxy: &gio::DBusProxy) -> Option<u32> {
    let params = glib::Variant::from(("org.freedesktop.appearance", "color-scheme"));
    let reply = proxy
        .call_sync(
            "Read",
            Some(&params),
            gio::DBusCallFlags::NONE,
            200,
            None::<&gio::Cancellable>,
        )
        .ok()?;
    let inner = reply.try_child_get::<glib::Variant>(0).ok().flatten()?;
    unpack_u32(&inner)
}

fn unpack_u32(value: &glib::Variant) -> Option<u32> {
    if let Some(number) = value.get::<u32>() {
        return Some(number);
    }
    value.as_variant().as_ref().and_then(unpack_u32)
}

fn hook_toplevel(window: &gtk::ApplicationWindow, paint: &Rc<Paint>, canvas: &gtk::DrawingArea) {
    if paint.toplevel_hooked.replace(true) {
        return;
    }
    let Some(surface) = window.surface() else {
        paint.toplevel_hooked.set(false);
        return;
    };
    let Ok(top) = surface.downcast::<gtk::gdk::Toplevel>() else {
        paint.toplevel_hooked.set(false);
        return;
    };
    let paint = Rc::clone(paint);
    let canvas = canvas.clone();
    let window = window.clone();
    top.connect_state_notify(move |_| {
        sync_ticks(&window, &paint, &canvas);
        if window.is_mapped() && !minimized(&window) {
            canvas.queue_draw();
        }
    });
}

fn sync_ticks(window: &gtk::ApplicationWindow, paint: &Rc<Paint>, canvas: &gtk::DrawingArea) {
    let run = window.is_mapped() && !minimized(window);
    if run {
        start_minute_tick(paint, canvas);
        start_scheme_tick(paint, canvas);
    } else {
        stop_tick(&paint.minute_tick);
        stop_tick(&paint.scheme_tick);
    }
}

fn start_minute_tick(paint: &Paint, canvas: &gtk::DrawingArea) {
    if paint.minute_tick.borrow().is_some() {
        return;
    }
    let canvas = canvas.clone();
    let id = glib::timeout_add_local(Duration::from_secs(60), move || {
        canvas.queue_draw();
        glib::ControlFlow::Continue
    });
    *paint.minute_tick.borrow_mut() = Some(id);
}

fn start_scheme_tick(paint: &Rc<Paint>, canvas: &gtk::DrawingArea) {
    if paint.scheme_tick.borrow().is_some() {
        return;
    }
    let paint_tick = Rc::clone(paint);
    let canvas = canvas.clone();
    let id = glib::timeout_add_local(Duration::from_millis(250), move || {
        let appearance = read_appearance(&paint_tick);
        if paint_tick.scheme.get() != Some(appearance) {
            paint_tick.scheme.set(Some(appearance));
            paint_tick.scheme_at.set(Some(Instant::now()));
            canvas.queue_draw();
        }
        glib::ControlFlow::Continue
    });
    *paint.scheme_tick.borrow_mut() = Some(id);
}

fn stop_tick(slot: &RefCell<Option<glib::SourceId>>) {
    if let Some(id) = slot.borrow_mut().take() {
        id.remove();
    }
}

fn minimized(window: &gtk::ApplicationWindow) -> bool {
    let Some(surface) = window.surface() else {
        return false;
    };
    let Ok(top) = surface.downcast::<gtk::gdk::Toplevel>() else {
        return false;
    };
    top.state().contains(gtk::gdk::ToplevelState::MINIMIZED)
}
