// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! One frame of the sun-arc pattern.
//!
//! A pure function of the surface size, the local minute, sunrise, sunset and the
//! owner's appearance. Nothing here reads the clock to choose light or dark, and
//! nothing here asks for a location or opens a network connection. The zone table
//! is the bundled copy of tzdb `zone.tab`.

use chrono::{Datelike, NaiveDate};

/// Published ratios. φ truncated as the pattern's token, and φ² the same way.
const DIAMETER_RATIO: f64 = 1.618_033_988_7;
const BOW_RATIO: f64 = 0.07919;
const TWILIGHT_RADIUS_RATIO: f64 = 2.618_033_988_7;
const GLOW_RADIUS_RATIO: f64 = 1.618_033_988_7;
const ENVELOPE_EDGE: f64 = 0.22;
const PEAK_LIGHT: f64 = 0.55;
const PEAK_DARK: f64 = 0.20;
const TWILIGHT_MINUTES: f64 = 30.0;
const GLOW_DAY_ALPHA: f64 = 0.22;
const TWILIGHT_ALPHA_DARK: f64 = 0.62;
const TWILIGHT_ALPHA_LIGHT: f64 = 0.95;
const TWILIGHT_SINK: f64 = 0.10;
const TRUE_DARK_MINUTES: f64 = 180.0;

const GROUND_LIGHT_DAY: Rgb = Rgb::new(0xFE, 0xFC, 0xF8);
const GROUND_LIGHT_NIGHT: Rgb = Rgb::new(0xAE, 0xA3, 0x93);
const GROUND_LIGHT_DEEP: Rgb = Rgb::new(0x8C, 0x80, 0x6E);
const GROUND_DARK_DAY: Rgb = Rgb::new(0x39, 0x2E, 0x26);
const GROUND_DARK_NIGHT: Rgb = Rgb::new(0x2E, 0x24, 0x1C);
const GROUND_DARK_DEEP: Rgb = Rgb::new(0x28, 0x1E, 0x17);
const GLOW_COLOR: Rgb = Rgb::new(0xFF, 0xCC, 0x33);
const TWILIGHT_DARK_TO: Rgb = Rgb::new(0xE8, 0x91, 0x3A);
const TWILIGHT_LIGHT_FROM: Rgb = Rgb::new(0xFF, 0xF3, 0xCF);

/// Rung 3, only when the zone identifier is not in the table.
pub const FALLBACK_RISE_MINUTES: f64 = 6.0 * 60.0 + 30.0;
pub const FALLBACK_SET_MINUTES: f64 = 19.0 * 60.0 + 30.0;

const ZONE_TABLE: &str = include_str!("sun_arc_zone_table.txt");

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Appearance {
    Light,
    Dark,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    /// `#RRGGBB`, the form a GTK CSS background accepts.
    #[must_use]
    pub fn css(self) -> String {
        format!("#{:02X}{:02X}{:02X}", self.r, self.g, self.b)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Glow {
    pub x: f64,
    pub y: f64,
    pub r: f64,
    pub a: f64,
    pub color: Rgb,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SunGeometry {
    pub r: f64,
    pub pos: Point,
    pub angle_deg: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SunFrame {
    pub t: f64,
    pub appearance: Appearance,
    pub ground: Rgb,
    pub on: bool,
    pub opacity: f64,
    pub geom: SunGeometry,
    pub halo: Option<Glow>,
    pub glow: Option<Glow>,
    /// Night amount, 0 through the day and 1 in the true night. The night sky is not drawn at 0.
    pub night: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SunTimes {
    pub rise: f64,
    pub set: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SampleKey {
    pub minute: i32,
    pub appearance: Appearance,
    pub width: u32,
    pub height: u32,
}

/// True when the minute, the appearance or the size changed. The caller passes the
/// local minute, so a tick inside the same minute is not a new sample.
#[must_use]
pub fn sample_due(previous: Option<SampleKey>, next: SampleKey) -> bool {
    previous != Some(next)
}

/// The zone's reference point, in degrees. `None` for an identifier the table does
/// not know, including a legacy alias. The caller then uses [`FALLBACK_RISE_MINUTES`].
#[must_use]
pub fn zone_point(zone_id: &str) -> Option<(f64, f64)> {
    if zone_id.is_empty() {
        return None;
    }
    for block in ZONE_TABLE.split('|') {
        let cut = block.find('/')?;
        let region = &block[..cut];
        for entry in block[cut + 1..].split(',') {
            let mut parts = entry.split(':');
            let city = parts.next()?;
            let lat: f64 = parts.next()?.parse().ok()?;
            let lon: f64 = parts.next()?.parse().ok()?;
            if parts.next().is_some() {
                continue;
            }
            let id = format!("{region}/{city}");
            if id == zone_id {
                return Some((lat / 100.0, lon / 100.0));
            }
        }
    }
    None
}

#[must_use]
pub fn zone_count() -> usize {
    let mut n = 0;
    for block in ZONE_TABLE.split('|') {
        let Some(cut) = block.find('/') else {
            continue;
        };
        n += block[cut + 1..]
            .split(',')
            .filter(|e| e.contains(':'))
            .count();
    }
    n
}

/// Sunrise and sunset in local minutes. A sunset at or before sunrise is carried
/// into the next day. Polar day or night walks back to the most recent valid day.
#[must_use]
pub fn solar_pair(
    lat: f64,
    lon: f64,
    date: NaiveDate,
    utc_offset_minutes: f64,
) -> Option<SunTimes> {
    let mut probe = date;
    for _ in 0..=183 {
        if let Some(times) = noaa_sunrise_sunset(lat, lon, probe, utc_offset_minutes) {
            let set = if times.set <= times.rise {
                times.set + 1440.0
            } else {
                times.set
            };
            return Some(SunTimes {
                rise: times.rise,
                set,
            });
        }
        probe = probe.pred_opt()?;
    }
    None
}

#[must_use]
pub fn frame(
    width: f64,
    height: f64,
    clock_minutes: f64,
    rise: f64,
    set_minutes: f64,
    appearance: Appearance,
) -> SunFrame {
    let set = if set_minutes < rise {
        set_minutes + 1440.0
    } else {
        set_minutes
    };
    let mut m = clock_minutes;
    if set + TWILIGHT_MINUTES > 1440.0 && m < set + TWILIGHT_MINUTES - 1440.0 {
        m += 1440.0;
    } else if rise - TWILIGHT_MINUTES < 0.0 && m >= rise - TWILIGHT_MINUTES + 1440.0 {
        m -= 1440.0;
    }
    let t = day_progress(m, rise, set);
    let env_val = env(t);
    let night = night_amount(m, rise, set);
    let twilight = twilight_weight(m, rise, set, env_val, t);
    let dark = appearance == Appearance::Dark;
    let on = (-0.02..=1.02).contains(&t);

    let grounds = if dark {
        (GROUND_DARK_DAY, GROUND_DARK_NIGHT, GROUND_DARK_DEEP)
    } else {
        (GROUND_LIGHT_DAY, GROUND_LIGHT_NIGHT, GROUND_LIGHT_DEEP)
    };
    let mixed = mix_oklab(grounds.0, grounds.1, night);
    let ground = mix_oklab(mixed, grounds.2, night * (1.0 - twilight.w));

    let clamped = t.clamp(0.0, 1.0);
    let geom = geometry(width, height, clamped);
    let peak = if dark { PEAK_DARK } else { PEAK_LIGHT };
    let mut opacity = sun_opacity(t, env_val, night, peak);
    if t <= 0.0 || t >= 1.0 {
        opacity *= 1.0 - night;
    }

    let halo = if on && night < 1.0 {
        Some(Glow {
            x: geom.pos.x,
            y: geom.pos.y,
            r: GLOW_RADIUS_RATIO * geom.r,
            a: GLOW_DAY_ALPHA * env_val * (1.0 - night),
            color: GLOW_COLOR,
        })
    } else {
        None
    };

    let glow = if twilight.w > 0.001 {
        let p = geometry(width, height, twilight.te).pos;
        let color = if dark {
            mix_oklab(GLOW_COLOR, TWILIGHT_DARK_TO, 0.30 + 0.50 * twilight.x)
        } else {
            mix_oklab(TWILIGHT_LIGHT_FROM, GLOW_COLOR, 0.45 + 0.25 * twilight.x)
        };
        Some(Glow {
            x: p.x,
            y: p.y,
            r: TWILIGHT_RADIUS_RATIO * geom.r,
            a: (if dark {
                TWILIGHT_ALPHA_DARK
            } else {
                TWILIGHT_ALPHA_LIGHT
            }) * twilight.w,
            color,
        })
    } else {
        None
    };

    SunFrame {
        t,
        appearance,
        ground,
        on,
        opacity,
        geom,
        halo,
        glow,
        night,
    }
}

struct Built {
    r: f64,
    pos: Point,
    angle_deg: f64,
}

fn geometry(width: f64, height: f64, t: f64) -> SunGeometry {
    let built = build_geometry(width, height, t);
    SunGeometry {
        r: built.r,
        pos: built.pos,
        angle_deg: built.angle_deg,
    }
}

fn build_geometry(width: f64, height: f64, t: f64) -> Built {
    let side = width.min(height);
    let r = DIAMETER_RATIO * side / 2.0;
    let delta = r / std::f64::consts::SQRT_2;
    let a = Point {
        x: -delta,
        y: -delta,
    };
    let b = Point {
        x: width + delta,
        y: height + delta,
    };
    let dx = b.x - a.x;
    let dy = b.y - a.y;
    let c = dx.hypot(dy);
    let s = BOW_RATIO * c;
    let r_arc = (c * c) / (8.0 * s) + s / 2.0;
    let mid = Point {
        x: (a.x + b.x) / 2.0,
        y: (a.y + b.y) / 2.0,
    };
    let ux = dx / c;
    let uy = dy / c;
    let (mut vx, mut vy) = (-uy, ux);
    if vy < 0.0 {
        vx = -vx;
        vy = -vy;
    }
    let origin = Point {
        x: mid.x + (r_arc - s) * vx,
        y: mid.y + (r_arc - s) * vy,
    };
    let theta_a = (a.y - origin.y).atan2(a.x - origin.x);
    let theta_b = (b.y - origin.y).atan2(b.x - origin.x);
    let angle_deg = 2.0 * (c / (2.0 * r_arc)).asin().to_degrees();
    let theta = theta_a + (theta_b - theta_a) * t;
    let pos = Point {
        x: origin.x + r_arc * theta.cos(),
        y: origin.y + r_arc * theta.sin(),
    };
    Built { r, pos, angle_deg }
}

fn env(t: f64) -> f64 {
    if !(0.0..=1.0).contains(&t) {
        return 0.0;
    }
    let e = ENVELOPE_EDGE;
    let val = if t < e {
        ((t / e) * std::f64::consts::FRAC_PI_2).sin()
    } else if t <= 1.0 - e {
        1.0
    } else {
        (((1.0 - t) / e) * std::f64::consts::FRAC_PI_2).sin()
    };
    val.clamp(0.0, 1.0)
}

fn night_amount(m: f64, rise: f64, set: f64) -> f64 {
    let dawn = rise - TWILIGHT_MINUTES;
    let dusk = set + TWILIGHT_MINUTES;
    if m < dawn {
        1.0
    } else if m < rise {
        (rise - m) / TWILIGHT_MINUTES
    } else if m <= set {
        0.0
    } else if m <= dusk {
        (m - set) / TWILIGHT_MINUTES
    } else {
        1.0
    }
}

fn day_progress(m: f64, rise: f64, set: f64) -> f64 {
    let dawn = rise - TWILIGHT_MINUTES;
    let dusk = set + TWILIGHT_MINUTES;
    (m - dawn) / (dusk - dawn)
}

fn sun_opacity(t: f64, env_val: f64, night: f64, peak: f64) -> f64 {
    if !(-0.02..=1.02).contains(&t) {
        return 0.0;
    }
    peak * env_val * (1.0 - night)
}

struct Twilight {
    w: f64,
    te: f64,
    x: f64,
}

fn wrap_minutes(v: f64) -> f64 {
    ((v % 1440.0) + 1440.0) % 1440.0
}

fn true_dark_half(span: f64) -> f64 {
    TRUE_DARK_MINUTES.min(span - 120.0).max(0.0) / 2.0
}

fn twilight_ease(x: f64) -> f64 {
    let c = x.clamp(0.0, 1.0);
    (1.0 - c * c).powf(1.5)
}

fn twilight_weight(m: f64, rise: f64, set: f64, env_val: f64, t: f64) -> Twilight {
    let dawn = rise - TWILIGHT_MINUTES;
    let dusk = set + TWILIGHT_MINUTES;
    if m >= dawn && m <= dusk {
        return Twilight {
            w: 1.0 - env_val,
            te: t,
            x: 0.0,
        };
    }
    let span = wrap_minutes(dawn - dusk);
    let ms = wrap_minutes(m - dusk);
    let mid = span / 2.0;
    let half = true_dark_half(span);
    let dark_start = mid - half;
    let dark_end = mid + half;
    if ms <= dark_start && dark_start > 0.0 {
        let x = ms / dark_start;
        return Twilight {
            w: twilight_ease(x),
            te: 1.0 + TWILIGHT_SINK * x,
            x,
        };
    }
    if ms >= dark_end && span > dark_end {
        let x = (span - ms) / (span - dark_end);
        return Twilight {
            w: twilight_ease(x),
            te: -TWILIGHT_SINK * x,
            x,
        };
    }
    Twilight {
        w: 0.0,
        te: if ms < mid {
            1.0 + TWILIGHT_SINK
        } else {
            -TWILIGHT_SINK
        },
        x: 1.0,
    }
}

fn srgb_to_linear(channel: f64) -> f64 {
    let c = channel / 255.0;
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn linear_to_srgb(channel: f64) -> f64 {
    let c = channel.clamp(0.0, 1.0);
    if c <= 0.0031308 {
        12.92 * c
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

fn rgb_to_oklab(color: Rgb) -> [f64; 3] {
    let lr = srgb_to_linear(f64::from(color.r));
    let lg = srgb_to_linear(f64::from(color.g));
    let lb = srgb_to_linear(f64::from(color.b));
    let l = 0.412_221_470_8 * lr + 0.536_332_536_3 * lg + 0.051_445_992_9 * lb;
    let m = 0.211_903_498_2 * lr + 0.680_699_545_1 * lg + 0.107_396_956_6 * lb;
    let s = 0.088_302_461_9 * lr + 0.281_718_837_6 * lg + 0.629_978_700_5 * lb;
    let l_ = l.cbrt();
    let m_ = m.cbrt();
    let s_ = s.cbrt();
    [
        0.210_454_255_3 * l_ + 0.793_617_785_0 * m_ - 0.004_072_046_8 * s_,
        1.977_998_495_1 * l_ - 2.428_592_205_0 * m_ + 0.450_593_709_9 * s_,
        0.025_904_037_1 * l_ + 0.782_771_766_2 * m_ - 0.808_675_766_0 * s_,
    ]
}

fn oklab_to_rgb(lab: [f64; 3]) -> Rgb {
    let [l_in, a, b] = lab;
    let l_ = l_in + 0.396_337_777_4 * a + 0.215_803_757_3 * b;
    let m_ = l_in - 0.105_561_345_8 * a - 0.063_854_172_8 * b;
    let s_ = l_in - 0.089_484_177_5 * a - 1.291_485_548_0 * b;
    let l = l_ * l_ * l_;
    let m = m_ * m_ * m_;
    let s = s_ * s_ * s_;
    let lr = 4.076_741_662_1 * l - 3.307_711_591_3 * m + 0.230_969_929_2 * s;
    let lg = -1.268_438_004_6 * l + 2.609_757_401_1 * m - 0.341_319_396_5 * s;
    let lb = -0.004_196_086_3 * l - 0.703_418_614_7 * m + 1.707_614_701_0 * s;
    Rgb {
        r: byte(linear_to_srgb(lr) * 255.0),
        g: byte(linear_to_srgb(lg) * 255.0),
        b: byte(linear_to_srgb(lb) * 255.0),
    }
}

fn byte(v: f64) -> u8 {
    let rounded = v.round();
    if rounded <= 0.0 {
        0
    } else if rounded >= 255.0 {
        255
    } else {
        // `rounded` is inside 1..=254.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        {
            rounded as u8
        }
    }
}

fn mix_oklab(a: Rgb, b: Rgb, t_raw: f64) -> Rgb {
    let t = t_raw.clamp(0.0, 1.0);
    let a = rgb_to_oklab(a);
    let b = rgb_to_oklab(b);
    oklab_to_rgb([
        a[0] + (b[0] - a[0]) * t,
        a[1] + (b[1] - a[1]) * t,
        a[2] + (b[2] - a[2]) * t,
    ])
}

fn noaa_sunrise_sunset(
    lat: f64,
    lon: f64,
    date: NaiveDate,
    utc_offset_minutes: f64,
) -> Option<SunTimes> {
    let n = f64::from(date.ordinal());
    let lng_hour = lon / 15.0;
    let calc = |is_rise: bool| -> Option<f64> {
        let t = n + ((if is_rise { 6.0 } else { 18.0 }) - lng_hour) / 24.0;
        let mean = 0.9856 * t - 3.289;
        let mrad = mean.to_radians();
        let mut ecl = mean + 1.916 * mrad.sin() + 0.020 * (2.0 * mrad).sin() + 282.634;
        ecl = wrap_degrees(ecl);
        let lrad = ecl.to_radians();
        let mut ra = (0.91764 * lrad.tan()).atan().to_degrees();
        ra = wrap_degrees(ra);
        let l_quad = (ecl / 90.0).floor() * 90.0;
        let ra_quad = (ra / 90.0).floor() * 90.0;
        ra = (ra + (l_quad - ra_quad)) / 15.0;
        let sin_dec = 0.39782 * lrad.sin();
        let cos_dec = sin_dec.asin().cos();
        let lat_rad = lat.to_radians();
        let cos_h =
            (90.833_f64.to_radians().cos() - sin_dec * lat_rad.sin()) / (cos_dec * lat_rad.cos());
        if !(-1.0..=1.0).contains(&cos_h) {
            return None;
        }
        let h = if is_rise {
            (360.0 - cos_h.acos().to_degrees()) / 15.0
        } else {
            cos_h.acos().to_degrees() / 15.0
        };
        let local_t = h + ra - 0.06571 * t - 6.622;
        let ut = wrap_hours(local_t - lng_hour);
        Some(wrap_minutes(ut * 60.0 + utc_offset_minutes))
    };
    Some(SunTimes {
        rise: calc(true)?,
        set: calc(false)?,
    })
}

fn wrap_degrees(v: f64) -> f64 {
    ((v % 360.0) + 360.0) % 360.0
}

fn wrap_hours(v: f64) -> f64 {
    ((v % 24.0) + 24.0) % 24.0
}

#[cfg(test)]
fn near(got: Rgb, expect: Rgb) -> bool {
    fn channel_near(got: u8, expect: u8) -> bool {
        got.abs_diff(expect) <= 1
    }
    channel_near(got.r, expect.r) && channel_near(got.g, expect.g) && channel_near(got.b, expect.b)
}

#[cfg(test)]
mod tests {
    use super::{
        Appearance, FALLBACK_RISE_MINUTES, FALLBACK_SET_MINUTES, GROUND_DARK_DAY, GROUND_DARK_DEEP,
        GROUND_DARK_NIGHT, GROUND_LIGHT_DAY, GROUND_LIGHT_DEEP, GROUND_LIGHT_NIGHT, SampleKey,
        frame, near, sample_due, solar_pair, zone_count, zone_point,
    };
    use chrono::NaiveDate;

    const RISE: f64 = 6.0 * 60.0 + 48.0;
    const SET: f64 = 18.0 * 60.0 + 56.0;

    fn denver_frame(minutes: f64, appearance: Appearance) -> super::SunFrame {
        frame(393.0, 852.0, minutes, RISE, SET, appearance)
    }

    #[test]
    fn ends_touch_the_corners_and_are_not_drawn() {
        let dawn = denver_frame(RISE - 30.0, Appearance::Light);
        let dusk = denver_frame(SET + 30.0, Appearance::Light);
        assert!((dawn.t).abs() < 1e-9);
        assert!((dusk.t - 1.0).abs() < 1e-9);
        assert_eq!(dawn.opacity, 0.0);
        assert_eq!(dusk.opacity, 0.0);
        let start = super::build_geometry(393.0, 852.0, 0.0);
        let end = super::build_geometry(393.0, 852.0, 1.0);
        let delta = start.r / std::f64::consts::SQRT_2;
        assert!((start.pos.x + delta).abs() < 1e-6);
        assert!((start.pos.y + delta).abs() < 1e-6);
        assert!((end.pos.x - (393.0 + delta)).abs() < 1e-6);
        assert!((end.pos.y - (852.0 + delta)).abs() < 1e-6);
        let tip = start.pos.x.hypot(start.pos.y);
        assert!((tip - start.r).abs() < 0.05, "tip {tip} radius {}", start.r);
        let far = (end.pos.x - 393.0).hypot(end.pos.y - 852.0);
        assert!((far - end.r).abs() < 0.05);
    }

    #[test]
    fn midday_centre_and_arc() {
        let g = super::build_geometry(393.0, 852.0, 0.5);
        assert!((g.pos.x - 299.6).abs() / 299.6 < 0.01);
        assert!((g.pos.y - 359.3).abs() / 359.3 < 0.01);
        assert!((g.angle_deg - 36.0).abs() < 0.1);
    }

    #[test]
    fn grounds_follow_the_appearance_argument() {
        let day = denver_frame(13.0 * 60.0, Appearance::Light);
        let evening = denver_frame(19.0 * 60.0 + 41.0, Appearance::Light);
        let deep = denver_frame(60.0, Appearance::Light);
        assert!(near(day.ground, GROUND_LIGHT_DAY), "{}", day.ground.css());
        assert!(
            near(evening.ground, GROUND_LIGHT_NIGHT),
            "{}",
            evening.ground.css()
        );
        assert!(
            near(deep.ground, GROUND_LIGHT_DEEP),
            "{}",
            deep.ground.css()
        );
        assert!(near(
            denver_frame(13.0 * 60.0, Appearance::Dark).ground,
            GROUND_DARK_DAY
        ));
        assert!(near(
            denver_frame(19.0 * 60.0 + 41.0, Appearance::Dark).ground,
            GROUND_DARK_NIGHT
        ));
        assert!(near(
            denver_frame(60.0, Appearance::Dark).ground,
            GROUND_DARK_DEEP
        ));
        assert_eq!(day.appearance, Appearance::Light);
        assert!(day.night < 0.01, "day draws no night sky");
        assert!(deep.night > 0.99);
        assert!(deep.glow.is_none());
    }

    #[test]
    fn denver_is_the_zone_table_and_an_alias_is_the_fallback() {
        assert_eq!(zone_count(), 418);
        let (lat, lon) = zone_point("America/Denver").expect("denver");
        assert!((lat - 39.74).abs() < 1e-9);
        assert!((lon + 104.98).abs() < 1e-9);
        assert!(zone_point("US/Mountain").is_none());
        assert!((FALLBACK_RISE_MINUTES - 390.0).abs() < 1e-9);
        assert!((FALLBACK_SET_MINUTES - 1170.0).abs() < 1e-9);
    }

    #[test]
    fn reykjavik_june_carries_sunset_past_midnight() {
        let (lat, lon) = zone_point("Atlantic/Reykjavik").expect("reykjavik");
        let date = NaiveDate::from_ymd_opt(2026, 6, 21).expect("date");
        let times = solar_pair(lat, lon, date, 0.0).expect("pair");
        assert!(times.set > 1440.0, "set {}", times.set);
        assert!(times.set > times.rise);
        let small_hours = frame(393.0, 852.0, 20.0, times.rise, times.set, Appearance::Light);
        assert!(small_hours.t < 1.0, "t {}", small_hours.t);
        assert!(small_hours.on);
    }

    #[test]
    fn a_sample_waits_for_the_next_minute_unless_appearance_or_size_changes() {
        let key = SampleKey {
            minute: 13 * 60,
            appearance: Appearance::Light,
            width: 1180,
            height: 760,
        };
        assert!(sample_due(None, key));
        assert!(!sample_due(Some(key), key));
        assert!(sample_due(
            Some(key),
            SampleKey {
                minute: key.minute + 1,
                ..key
            }
        ));
        assert!(sample_due(
            Some(key),
            SampleKey {
                appearance: Appearance::Dark,
                ..key
            }
        ));
        assert!(sample_due(
            Some(key),
            SampleKey {
                width: key.width + 1,
                ..key
            }
        ));
    }
}
