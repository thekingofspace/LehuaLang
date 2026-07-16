use std::cell::RefCell;
use std::io::Cursor;
use std::rc::Rc;

use ab_glyph::{FontVec, PxScale};
use image::imageops::FilterType;
use image::{imageops, DynamicImage, ImageFormat, Rgba, RgbaImage};
use imageproc::drawing;
use imageproc::geometric_transformations::{rotate_about_center, translate, Interpolation};
use imageproc::point::Point;
use imageproc::rect::Rect;
use mlua::{AnyUserData, Function, Lua, MetaMethod, Table, UserData, UserDataMethods, Value};

use super::{LibCtx, PathScope};
use crate::error::LehuaError;

pub struct Canvas {
    pub img: RefCell<RgbaImage>,
}

pub struct FontObj {
    font: FontVec,
}

pub fn from_image(img: RgbaImage) -> Canvas {
    Canvas {
        img: RefCell::new(img),
    }
}

pub fn from_raw_pixels(width: u32, height: u32, pixels: Vec<u8>) -> mlua::Result<Canvas> {
    let img = RgbaImage::from_raw(width, height, pixels)
        .ok_or_else(|| LehuaError::msg("canvas: pixel data does not match the given dimensions"))?;
    Ok(from_image(img))
}

pub fn decode_bytes(bytes: &[u8]) -> mlua::Result<Canvas> {
    let img = image::load_from_memory(bytes)
        .map_err(|e| LehuaError::msg(format!("could not decode image: {e}")))?;
    Ok(from_image(img.to_rgba8()))
}

pub fn format_from_name(name: &str) -> mlua::Result<ImageFormat> {
    match name.trim().trim_start_matches('.').to_ascii_lowercase().as_str() {
        "png" => Ok(ImageFormat::Png),
        "jpg" | "jpeg" => Ok(ImageFormat::Jpeg),
        "gif" => Ok(ImageFormat::Gif),
        "bmp" => Ok(ImageFormat::Bmp),
        "ico" => Ok(ImageFormat::Ico),
        "tiff" | "tif" => Ok(ImageFormat::Tiff),
        "webp" => Ok(ImageFormat::WebP),
        "qoi" => Ok(ImageFormat::Qoi),
        "tga" => Ok(ImageFormat::Tga),
        other => Err(LehuaError::msg(format!(
            "unknown image format '{other}' (supported: png, jpg, gif, bmp, ico, tiff, webp, qoi, tga)"
        ))
        .into()),
    }
}

pub fn encode_image(img: &RgbaImage, format: ImageFormat, quality: Option<u8>) -> mlua::Result<Vec<u8>> {
    let mut out = Cursor::new(Vec::new());
    let result = match format {
        ImageFormat::Jpeg => {
            let rgb = DynamicImage::ImageRgba8(img.clone()).to_rgb8();
            let q = quality.unwrap_or(90).clamp(1, 100);
            let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, q);
            rgb.write_with_encoder(encoder)
        }
        ImageFormat::Ico => {
            let resized = if img.width() > 256 || img.height() > 256 {
                imageops::resize(img, img.width().min(256), img.height().min(256), FilterType::Lanczos3)
            } else {
                img.clone()
            };
            resized.write_to(&mut out, format)
        }
        _ => img.write_to(&mut out, format),
    };
    result.map_err(|e| LehuaError::msg(format!("could not encode image: {e}")))?;
    Ok(out.into_inner())
}

fn named_color(name: &str) -> Option<[u8; 4]> {
    let c = match name {
        "black" => [0, 0, 0, 255],
        "white" => [255, 255, 255, 255],
        "red" => [255, 0, 0, 255],
        "green" => [0, 128, 0, 255],
        "lime" => [0, 255, 0, 255],
        "blue" => [0, 0, 255, 255],
        "yellow" => [255, 255, 0, 255],
        "cyan" | "aqua" => [0, 255, 255, 255],
        "magenta" | "fuchsia" => [255, 0, 255, 255],
        "gray" | "grey" => [128, 128, 128, 255],
        "silver" => [192, 192, 192, 255],
        "maroon" => [128, 0, 0, 255],
        "olive" => [128, 128, 0, 255],
        "teal" => [0, 128, 128, 255],
        "navy" => [0, 0, 128, 255],
        "purple" => [128, 0, 128, 255],
        "orange" => [255, 165, 0, 255],
        "brown" => [165, 42, 42, 255],
        "pink" => [255, 192, 203, 255],
        "gold" => [255, 215, 0, 255],
        "violet" => [238, 130, 238, 255],
        "indigo" => [75, 0, 130, 255],
        "transparent" => [0, 0, 0, 0],
        _ => return None,
    };
    Some(c)
}

fn parse_hex(s: &str) -> Option<[u8; 4]> {
    let s = s.trim().trim_start_matches('#');
    let hex = |a: u8| -> Option<u8> {
        match a {
            b'0'..=b'9' => Some(a - b'0'),
            b'a'..=b'f' => Some(a - b'a' + 10),
            b'A'..=b'F' => Some(a - b'A' + 10),
            _ => None,
        }
    };
    let b = s.as_bytes();
    match b.len() {
        3 | 4 => {
            let mut out = [0u8; 4];
            out[3] = 255;
            for i in 0..b.len() {
                let v = hex(b[i])?;
                out[i] = v * 17;
            }
            Some(out)
        }
        6 | 8 => {
            let mut out = [0u8; 4];
            out[3] = 255;
            for i in 0..b.len() / 2 {
                out[i] = hex(b[i * 2])? * 16 + hex(b[i * 2 + 1])?;
            }
            Some(out)
        }
        _ => None,
    }
}

fn clamp_channel(v: f64) -> u8 {
    v.round().clamp(0.0, 255.0) as u8
}

pub fn parse_color(v: &Value) -> mlua::Result<Rgba<u8>> {
    match v {
        Value::String(s) => {
            let text = s.to_str()?.to_string();
            let lowered = text.trim().to_ascii_lowercase();
            if let Some(c) = named_color(&lowered) {
                return Ok(Rgba(c));
            }
            if let Some(c) = parse_hex(&text) {
                return Ok(Rgba(c));
            }
            Err(LehuaError::msg(format!("invalid color '{text}'")).into())
        }
        Value::Table(t) => {
            let get = |keys: (&str, i64)| -> mlua::Result<Option<f64>> {
                if let Some(v) = t.get::<Option<f64>>(keys.0)? {
                    return Ok(Some(v));
                }
                t.raw_get::<Option<f64>>(keys.1)
            };
            let r = get(("r", 1))?.unwrap_or(0.0);
            let g = get(("g", 2))?.unwrap_or(0.0);
            let b = get(("b", 3))?.unwrap_or(0.0);
            let a = get(("a", 4))?.unwrap_or(255.0);
            Ok(Rgba([
                clamp_channel(r),
                clamp_channel(g),
                clamp_channel(b),
                clamp_channel(a),
            ]))
        }
        other => Err(LehuaError::msg(format!(
            "expected a color (hex string, name, or {{r, g, b, a}} table), got {}",
            other.type_name()
        ))
        .into()),
    }
}

fn opt_color(v: &Option<Value>, default: Rgba<u8>) -> mlua::Result<Rgba<u8>> {
    match v {
        Some(val) if !val.is_nil() => parse_color(val),
        _ => Ok(default),
    }
}

fn color_to_table(lua: &Lua, c: Rgba<u8>) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    t.set("r", c.0[0])?;
    t.set("g", c.0[1])?;
    t.set("b", c.0[2])?;
    t.set("a", c.0[3])?;
    Ok(t)
}

fn hsv_to_rgb(h: f64, s: f64, v: f64) -> [u8; 3] {
    let h = ((h % 360.0) + 360.0) % 360.0;
    let s = s.clamp(0.0, 1.0);
    let v = v.clamp(0.0, 1.0);
    let c = v * s;
    let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
    let m = v - c;
    let (r, g, b) = match (h / 60.0) as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    [
        clamp_channel((r + m) * 255.0),
        clamp_channel((g + m) * 255.0),
        clamp_channel((b + m) * 255.0),
    ]
}

fn parse_points(v: &Value) -> mlua::Result<Vec<(f32, f32)>> {
    let t = match v {
        Value::Table(t) => t,
        other => {
            return Err(LehuaError::msg(format!(
                "expected a table of points, got {}",
                other.type_name()
            ))
            .into())
        }
    };
    let len = t.raw_len();
    if len == 0 {
        return Ok(Vec::new());
    }
    let first: Value = t.raw_get(1)?;
    let mut out = Vec::new();
    match first {
        Value::Table(_) => {
            for i in 1..=len {
                let p: Table = t.raw_get(i)?;
                let x = p
                    .get::<Option<f32>>("x")?
                    .or(p.raw_get::<Option<f32>>(1)?)
                    .ok_or_else(|| LehuaError::msg("point is missing x"))?;
                let y = p
                    .get::<Option<f32>>("y")?
                    .or(p.raw_get::<Option<f32>>(2)?)
                    .ok_or_else(|| LehuaError::msg("point is missing y"))?;
                out.push((x, y));
            }
        }
        _ => {
            if len % 2 != 0 {
                return Err(LehuaError::msg(
                    "a flat point list needs an even number of coordinates",
                )
                .into());
            }
            for i in (1..=len).step_by(2) {
                let x: f32 = t.raw_get(i)?;
                let y: f32 = t.raw_get(i + 1)?;
                out.push((x, y));
            }
        }
    }
    Ok(out)
}

fn polygon_points(pts: &[(f32, f32)]) -> mlua::Result<Vec<Point<i32>>> {
    if pts.iter().any(|(x, y)| !x.is_finite() || !y.is_finite()) {
        return Err(LehuaError::msg("polygon points must be finite numbers").into());
    }
    let mut out: Vec<Point<i32>> = pts
        .iter()
        .map(|(x, y)| Point::new(x.round() as i32, y.round() as i32))
        .collect();
    out.dedup();
    if out.len() > 1 && out.first() == out.last() {
        out.pop();
    }
    if out.len() < 3 {
        return Err(LehuaError::msg("a polygon needs at least 3 distinct points").into());
    }
    Ok(out)
}

struct GradientStop {
    offset: f32,
    color: Rgba<u8>,
}

fn parse_stops(v: &Value) -> mlua::Result<Vec<GradientStop>> {
    let t = match v {
        Value::Table(t) => t,
        other => {
            return Err(LehuaError::msg(format!(
                "expected a table of gradient stops, got {}",
                other.type_name()
            ))
            .into())
        }
    };
    let len = t.raw_len();
    if len < 2 {
        return Err(LehuaError::msg("a gradient needs at least 2 stops").into());
    }
    let mut raw: Vec<(Option<f32>, Rgba<u8>)> = Vec::with_capacity(len);
    for i in 1..=len {
        let entry: Value = t.raw_get(i)?;
        match &entry {
            Value::Table(et) => {
                let offset = et.get::<Option<f32>>("offset")?;
                let color_field: Option<Value> = et.get("color")?;
                if offset.is_some() || color_field.is_some() {
                    let c = color_field
                        .ok_or_else(|| LehuaError::msg("gradient stop is missing its color"))?;
                    raw.push((offset, parse_color(&c)?));
                    continue;
                }
                if et.raw_len() == 2 {
                    let first: Value = et.raw_get(1)?;
                    if let Value::Number(_) | Value::Integer(_) = first {
                        let off: f32 = et.raw_get(1)?;
                        let c: Value = et.raw_get(2)?;
                        raw.push((Some(off), parse_color(&c)?));
                        continue;
                    }
                }
                raw.push((None, parse_color(&entry)?));
            }
            _ => raw.push((None, parse_color(&entry)?)),
        }
    }
    let n = raw.len();
    let mut out = Vec::with_capacity(n);
    for (i, (off, color)) in raw.into_iter().enumerate() {
        let offset = match off {
            Some(o) if o.is_nan() => {
                return Err(LehuaError::msg("a gradient stop offset cannot be nan").into())
            }
            Some(o) => o.clamp(0.0, 1.0),
            None => i as f32 / (n - 1) as f32,
        };
        out.push(GradientStop { offset, color });
    }
    out.sort_by(|a, b| a.offset.total_cmp(&b.offset));
    Ok(out)
}

fn gradient_color(stops: &[GradientStop], t: f32) -> Rgba<u8> {
    let t = t.clamp(0.0, 1.0);
    if t <= stops[0].offset {
        return stops[0].color;
    }
    for pair in stops.windows(2) {
        let (a, b) = (&pair[0], &pair[1]);
        if t <= b.offset {
            let span = (b.offset - a.offset).max(1e-6);
            let f = (t - a.offset) / span;
            let mut out = [0u8; 4];
            for i in 0..4 {
                out[i] =
                    clamp_channel(a.color.0[i] as f64 + (b.color.0[i] as f64 - a.color.0[i] as f64) * f as f64);
            }
            return Rgba(out);
        }
    }
    stops[stops.len() - 1].color
}

#[derive(Clone, Copy, PartialEq)]
enum Blend {
    Normal,
    Add,
    Subtract,
    Multiply,
    Screen,
    Overlay,
    Darken,
    Lighten,
    Difference,
    Exclusion,
}

fn parse_blend(name: Option<String>) -> mlua::Result<Blend> {
    let name = match name {
        Some(n) => n,
        None => return Ok(Blend::Normal),
    };
    match name.trim().to_ascii_lowercase().as_str() {
        "normal" | "over" => Ok(Blend::Normal),
        "add" => Ok(Blend::Add),
        "subtract" => Ok(Blend::Subtract),
        "multiply" => Ok(Blend::Multiply),
        "screen" => Ok(Blend::Screen),
        "overlay" => Ok(Blend::Overlay),
        "darken" => Ok(Blend::Darken),
        "lighten" => Ok(Blend::Lighten),
        "difference" => Ok(Blend::Difference),
        "exclusion" => Ok(Blend::Exclusion),
        other => Err(LehuaError::msg(format!(
            "unknown blend mode '{other}' (normal, add, subtract, multiply, screen, overlay, darken, lighten, difference, exclusion)"
        ))
        .into()),
    }
}

fn blend_channel(mode: Blend, d: f64, s: f64) -> f64 {
    match mode {
        Blend::Normal => s,
        Blend::Add => (d + s).min(255.0),
        Blend::Subtract => (d - s).max(0.0),
        Blend::Multiply => d * s / 255.0,
        Blend::Screen => 255.0 - (255.0 - d) * (255.0 - s) / 255.0,
        Blend::Overlay => {
            if d < 128.0 {
                2.0 * d * s / 255.0
            } else {
                255.0 - 2.0 * (255.0 - d) * (255.0 - s) / 255.0
            }
        }
        Blend::Darken => d.min(s),
        Blend::Lighten => d.max(s),
        Blend::Difference => (d - s).abs(),
        Blend::Exclusion => d + s - 2.0 * d * s / 255.0,
    }
}

fn composite_pixel(dst: Rgba<u8>, src: Rgba<u8>, mode: Blend, opacity: f64) -> Rgba<u8> {
    let opacity = if opacity.is_finite() {
        opacity.clamp(0.0, 1.0)
    } else {
        1.0
    };
    let sa = src.0[3] as f64 / 255.0 * opacity;
    if sa <= 0.0 {
        return dst;
    }
    let da = dst.0[3] as f64 / 255.0;
    let out_a = sa + da * (1.0 - sa);
    if out_a <= 0.0 {
        return Rgba([0, 0, 0, 0]);
    }
    let mut out = [0u8; 4];
    for i in 0..3 {
        let d = dst.0[i] as f64;
        let s = src.0[i] as f64;
        let blended = s * (1.0 - da) + blend_channel(mode, d, s) * da;
        let src_part = blended * sa;
        let dst_part = d * da * (1.0 - sa);
        out[i] = clamp_channel((src_part + dst_part) / out_a);
    }
    out[3] = clamp_channel(out_a * 255.0);
    Rgba(out)
}

fn luminance(p: &Rgba<u8>) -> f64 {
    0.2126 * p.0[0] as f64 + 0.7152 * p.0[1] as f64 + 0.0722 * p.0[2] as f64
}

fn draw_composited<F>(img: &mut RgbaImage, color: Rgba<u8>, draw: F)
where
    F: FnOnce(&mut RgbaImage, Rgba<u8>),
{
    if color.0[3] == 255 {
        draw(img, color);
        return;
    }
    if color.0[3] == 0 {
        return;
    }
    let mut layer = RgbaImage::new(img.width(), img.height());
    let opaque = Rgba([color.0[0], color.0[1], color.0[2], 255]);
    draw(&mut layer, opaque);
    let alpha = color.0[3] as u32;
    for (x, y, sp) in layer.enumerate_pixels() {
        if sp.0[3] == 0 {
            continue;
        }
        let mut src = *sp;
        src.0[3] = (src.0[3] as u32 * alpha / 255) as u8;
        let dp = *img.get_pixel(x, y);
        img.put_pixel(x, y, composite_pixel(dp, src, Blend::Normal, 1.0));
    }
}

fn shadow_layer(src: &RgbaImage, dx: i64, dy: i64, sigma: f32, color: Rgba<u8>, strength: f64) -> RgbaImage {
    let (w, h) = src.dimensions();
    let mut shadow = RgbaImage::new(w, h);
    let strength = strength.clamp(0.0, 4.0);
    for (x, y, p) in shadow.enumerate_pixels_mut() {
        let sx = x as i64 - dx;
        let sy = y as i64 - dy;
        if sx < 0 || sy < 0 || sx as u32 >= w || sy as u32 >= h {
            continue;
        }
        let a = src.get_pixel(sx as u32, sy as u32).0[3] as f64;
        let a = a / 255.0 * color.0[3] as f64 * strength;
        *p = Rgba([color.0[0], color.0[1], color.0[2], clamp_channel(a)]);
    }
    if sigma > 0.05 {
        imageproc::filter::gaussian_blur_f32(&shadow, sigma)
    } else {
        shadow
    }
}

fn under_composite(img: &mut RgbaImage, mut under: RgbaImage) {
    for (x, y, p) in img.enumerate_pixels() {
        let base = *under.get_pixel(x, y);
        under.put_pixel(x, y, composite_pixel(base, *p, Blend::Normal, 1.0));
    }
    *img = under;
}

fn dilate_alpha(src: &RgbaImage, radius: u32) -> Vec<u8> {
    let (w, h) = src.dimensions();
    let r = radius as i64;
    let mut out = vec![0u8; (w * h) as usize];
    let mut offsets = Vec::new();
    for oy in -r..=r {
        for ox in -r..=r {
            if ox * ox + oy * oy <= r * r {
                offsets.push((ox, oy));
            }
        }
    }
    for y in 0..h as i64 {
        for x in 0..w as i64 {
            let mut best = 0u8;
            for (ox, oy) in &offsets {
                let sx = x + ox;
                let sy = y + oy;
                if sx < 0 || sy < 0 || sx >= w as i64 || sy >= h as i64 {
                    continue;
                }
                let a = src.get_pixel(sx as u32, sy as u32).0[3];
                if a > best {
                    best = a;
                    if best == 255 {
                        break;
                    }
                }
            }
            out[(y as u32 * w + x as u32) as usize] = best;
        }
    }
    out
}

fn draw_text_line(
    img: &mut RgbaImage,
    color: Rgba<u8>,
    x: i32,
    y: i32,
    scale: PxScale,
    font: &FontVec,
    line: &str,
    spacing: f32,
) {
    if spacing.abs() < 0.01 {
        drawing::draw_text_mut(img, color, x, y, scale, font, line);
        return;
    }
    let mut cursor = x as f32;
    let mut buf = [0u8; 4];
    for ch in line.chars() {
        let s = ch.encode_utf8(&mut buf);
        let (w, _) = drawing::text_size(scale, font, s);
        drawing::draw_text_mut(img, color, cursor.round() as i32, y, scale, font, s);
        let advance = if w == 0 { scale.x / 3.0 } else { w as f32 };
        cursor += advance + spacing;
    }
}

fn thick_line(img: &mut RgbaImage, x1: f32, y1: f32, x2: f32, y2: f32, color: Rgba<u8>, thickness: f32) {
    if !(x1.is_finite() && y1.is_finite() && x2.is_finite() && y2.is_finite()) {
        return;
    }
    let max_dim = (img.width().max(img.height()) as f32) * 4.0 + 64.0;
    let thickness = if thickness.is_finite() {
        thickness.min(max_dim)
    } else {
        1.0
    };
    if thickness <= 1.5 {
        drawing::draw_line_segment_mut(img, (x1, y1), (x2, y2), color);
        return;
    }
    let half = thickness / 2.0;
    let dx = x2 - x1;
    let dy = y2 - y1;
    let len = (dx * dx + dy * dy).sqrt();
    if len < 0.5 {
        drawing::draw_filled_circle_mut(img, (x1.round() as i32, y1.round() as i32), half.round() as i32, color);
        return;
    }
    let nx = -dy / len * half;
    let ny = dx / len * half;
    let quad = vec![
        Point::new((x1 + nx).round() as i32, (y1 + ny).round() as i32),
        Point::new((x2 + nx).round() as i32, (y2 + ny).round() as i32),
        Point::new((x2 - nx).round() as i32, (y2 - ny).round() as i32),
        Point::new((x1 - nx).round() as i32, (y1 - ny).round() as i32),
    ];
    let mut quad_dedup = quad.clone();
    quad_dedup.dedup();
    if quad_dedup.len() >= 3 && quad_dedup.first() != quad_dedup.last() {
        drawing::draw_polygon_mut(img, &quad_dedup, color);
    }
    let r = half.round() as i32;
    drawing::draw_filled_circle_mut(img, (x1.round() as i32, y1.round() as i32), r, color);
    drawing::draw_filled_circle_mut(img, (x2.round() as i32, y2.round() as i32), r, color);
}

fn stroke_arc(
    img: &mut RgbaImage,
    cx: f32,
    cy: f32,
    r: f32,
    start_deg: f32,
    end_deg: f32,
    color: Rgba<u8>,
    thickness: f32,
) {
    let start = start_deg.to_radians();
    let end = end_deg.to_radians();
    let sweep = end - start;
    let steps = ((r.abs() * sweep.abs()).ceil() as usize).clamp(8, 720);
    let mut prev = (cx + r * start.cos(), cy + r * start.sin());
    for i in 1..=steps {
        let a = start + sweep * i as f32 / steps as f32;
        let next = (cx + r * a.cos(), cy + r * a.sin());
        thick_line(img, prev.0, prev.1, next.0, next.1, color, thickness);
        prev = next;
    }
}

fn convolve3x3(img: &RgbaImage, kernel: [f64; 9], bias: f64) -> RgbaImage {
    let (w, h) = img.dimensions();
    let mut out = img.clone();
    for y in 0..h {
        for x in 0..w {
            let mut acc = [0.0f64; 3];
            for ky in 0..3i64 {
                for kx in 0..3i64 {
                    let sx = (x as i64 + kx - 1).clamp(0, w as i64 - 1) as u32;
                    let sy = (y as i64 + ky - 1).clamp(0, h as i64 - 1) as u32;
                    let p = img.get_pixel(sx, sy);
                    let k = kernel[(ky * 3 + kx) as usize];
                    for c in 0..3 {
                        acc[c] += p.0[c] as f64 * k;
                    }
                }
            }
            let a = img.get_pixel(x, y).0[3];
            out.put_pixel(
                x,
                y,
                Rgba([
                    clamp_channel(acc[0] + bias),
                    clamp_channel(acc[1] + bias),
                    clamp_channel(acc[2] + bias),
                    a,
                ]),
            );
        }
    }
    out
}

fn sample_bilinear(img: &RgbaImage, x: f64, y: f64) -> Rgba<u8> {
    let (w, h) = img.dimensions();
    if x < -1.0 || y < -1.0 || x > w as f64 || y > h as f64 {
        return Rgba([0, 0, 0, 0]);
    }
    let x0 = x.floor();
    let y0 = y.floor();
    let fx = x - x0;
    let fy = y - y0;
    let get = |px: f64, py: f64| -> [f64; 4] {
        if px < 0.0 || py < 0.0 || px >= w as f64 || py >= h as f64 {
            return [0.0; 4];
        }
        let p = img.get_pixel(px as u32, py as u32);
        [p.0[0] as f64, p.0[1] as f64, p.0[2] as f64, p.0[3] as f64]
    };
    let p00 = get(x0, y0);
    let p10 = get(x0 + 1.0, y0);
    let p01 = get(x0, y0 + 1.0);
    let p11 = get(x0 + 1.0, y0 + 1.0);
    let mut out = [0u8; 4];
    for i in 0..4 {
        let top = p00[i] * (1.0 - fx) + p10[i] * fx;
        let bottom = p01[i] * (1.0 - fx) + p11[i] * fx;
        out[i] = clamp_channel(top * (1.0 - fy) + bottom * fy);
    }
    Rgba(out)
}

fn displace<F>(img: &mut RgbaImage, map: F)
where
    F: Fn(f64, f64) -> (f64, f64),
{
    let src = img.clone();
    for (x, y, p) in img.enumerate_pixels_mut() {
        let (sx, sy) = map(x as f64, y as f64);
        *p = sample_bilinear(&src, sx, sy);
    }
}

fn channel_index(name: &str) -> mlua::Result<usize> {
    match name.trim().to_ascii_lowercase().as_str() {
        "r" | "red" => Ok(0),
        "g" | "green" => Ok(1),
        "b" | "blue" => Ok(2),
        "a" | "alpha" => Ok(3),
        other => Err(LehuaError::msg(format!(
            "unknown channel '{other}' (r, g, b, or a)"
        ))
        .into()),
    }
}

fn filter_name(name: Option<String>) -> mlua::Result<FilterType> {
    let name = match name {
        Some(n) => n,
        None => return Ok(FilterType::Lanczos3),
    };
    match name.trim().to_ascii_lowercase().as_str() {
        "nearest" => Ok(FilterType::Nearest),
        "linear" | "triangle" | "bilinear" => Ok(FilterType::Triangle),
        "cubic" | "catmullrom" | "bicubic" => Ok(FilterType::CatmullRom),
        "gaussian" => Ok(FilterType::Gaussian),
        "lanczos" => Ok(FilterType::Lanczos3),
        other => Err(LehuaError::msg(format!(
            "unknown resize filter '{other}' (nearest, linear, cubic, gaussian, lanczos)"
        ))
        .into()),
    }
}

fn borrow_canvas(ud: &AnyUserData) -> mlua::Result<mlua::UserDataRef<Canvas>> {
    ud.borrow::<Canvas>()
        .map_err(|_| LehuaError::msg("expected a canvas").into())
}

fn data_bytes(v: &Value, what: &str) -> mlua::Result<Vec<u8>> {
    match v {
        Value::Buffer(b) => Ok(b.to_vec()),
        Value::String(s) => Ok(s.as_bytes().to_vec()),
        other => Err(LehuaError::msg(format!(
            "{what} expects a buffer or string of RGBA bytes, got {}",
            other.type_name()
        ))
        .into()),
    }
}

struct Lcg(u64);

impl Lcg {
    fn new() -> Self {
        let mut b = [0u8; 8];
        let _ = getrandom::fill(&mut b);
        Lcg(u64::from_le_bytes(b) | 1)
    }
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.0 >> 33
    }
    fn next_signed(&mut self, amount: f64) -> f64 {
        (self.next() as f64 / (1u64 << 30) as f64 - 1.0) * amount
    }
}

#[derive(Clone, Copy, PartialEq)]
enum NoiseKind {
    Perlin,
    Value,
    Worley,
    Ridged,
}

fn parse_noise_kind(name: Option<String>) -> mlua::Result<NoiseKind> {
    let name = match name {
        Some(n) => n,
        None => return Ok(NoiseKind::Perlin),
    };
    match name.trim().to_ascii_lowercase().as_str() {
        "perlin" => Ok(NoiseKind::Perlin),
        "value" => Ok(NoiseKind::Value),
        "worley" | "cellular" => Ok(NoiseKind::Worley),
        "ridged" => Ok(NoiseKind::Ridged),
        other => Err(LehuaError::msg(format!(
            "unknown noise kind '{other}' (perlin, value, worley, ridged)"
        ))
        .into()),
    }
}

#[derive(Clone, Copy)]
struct NoiseParams {
    seed: u64,
    scale: f64,
    octaves: u32,
    persistence: f64,
    lacunarity: f64,
    kind: NoiseKind,
}

pub struct NoiseCore {
    perm: RefCell<[u8; 512]>,
    params: RefCell<NoiseParams>,
    warp: RefCell<Option<(Rc<NoiseCore>, f64)>>,
}

struct NoiseSpec {
    perm: [u8; 512],
    params: NoiseParams,
    warp: Option<(Box<NoiseSpec>, f64)>,
}

fn snapshot_core(core: &NoiseCore) -> NoiseSpec {
    NoiseSpec {
        perm: *core.perm.borrow(),
        params: *core.params.borrow(),
        warp: core
            .warp
            .borrow()
            .as_ref()
            .map(|(c, s)| (Box::new(snapshot_core(c)), *s)),
    }
}

fn rebuild_core(spec: NoiseSpec) -> Rc<NoiseCore> {
    Rc::new(NoiseCore {
        perm: RefCell::new(spec.perm),
        params: RefCell::new(spec.params),
        warp: RefCell::new(spec.warp.map(|(b, s)| (rebuild_core(*b), s))),
    })
}

fn splitmix64(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9e3779b97f4a7c15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
    z ^ (z >> 31)
}

fn build_perm(seed: u64) -> [u8; 512] {
    let mut state = seed;
    let mut table: [u8; 256] = [0; 256];
    for (i, v) in table.iter_mut().enumerate() {
        *v = i as u8;
    }
    for i in (1..256).rev() {
        let j = (splitmix64(&mut state) % (i as u64 + 1)) as usize;
        table.swap(i, j);
    }
    let mut out = [0u8; 512];
    out[..256].copy_from_slice(&table);
    out[256..].copy_from_slice(&table);
    out
}

fn fade(t: f64) -> f64 {
    t * t * t * (t * (t * 6.0 - 15.0) + 10.0)
}

fn grad2(hash: u8, x: f64, y: f64) -> f64 {
    match hash & 7 {
        0 => x + y,
        1 => x - y,
        2 => -x + y,
        3 => -x - y,
        4 => x,
        5 => -x,
        6 => y,
        _ => -y,
    }
}

impl NoiseCore {
    fn perlin(&self, x: f64, y: f64) -> f64 {
        let perm = self.perm.borrow();
        let xi = x.floor();
        let yi = y.floor();
        let xf = x - xi;
        let yf = y - yi;
        let xw = (xi as i64 & 255) as usize;
        let yw = (yi as i64 & 255) as usize;
        let u = fade(xf);
        let v = fade(yf);
        let aa = perm[perm[xw] as usize + yw];
        let ab = perm[perm[xw] as usize + yw + 1];
        let ba = perm[perm[xw + 1] as usize + yw];
        let bb = perm[perm[xw + 1] as usize + yw + 1];
        let x1 = grad2(aa, xf, yf) + u * (grad2(ba, xf - 1.0, yf) - grad2(aa, xf, yf));
        let x2 = grad2(ab, xf, yf - 1.0)
            + u * (grad2(bb, xf - 1.0, yf - 1.0) - grad2(ab, xf, yf - 1.0));
        (x1 + v * (x2 - x1)) * std::f64::consts::FRAC_1_SQRT_2
    }

    fn lattice_value(&self, xi: i64, yi: i64) -> f64 {
        let perm = self.perm.borrow();
        let h = perm[perm[(xi & 255) as usize] as usize + (yi & 255) as usize];
        h as f64 / 255.0
    }

    fn value(&self, x: f64, y: f64) -> f64 {
        let xi = x.floor();
        let yi = y.floor();
        let u = fade(x - xi);
        let v = fade(y - yi);
        let xi = xi as i64;
        let yi = yi as i64;
        let a = self.lattice_value(xi, yi);
        let b = self.lattice_value(xi + 1, yi);
        let c = self.lattice_value(xi, yi + 1);
        let d = self.lattice_value(xi + 1, yi + 1);
        let top = a + u * (b - a);
        let bottom = c + u * (d - c);
        (top + v * (bottom - top)) * 2.0 - 1.0
    }

    fn feature_point(&self, xi: i64, yi: i64) -> (f64, f64) {
        let perm = self.perm.borrow();
        let hx = perm[perm[(xi & 255) as usize] as usize + (yi & 255) as usize] as f64 / 255.0;
        let hy = perm[perm[(yi & 255) as usize] as usize + ((xi + 89) & 255) as usize] as f64 / 255.0;
        (xi as f64 + hx, yi as f64 + hy)
    }

    fn worley(&self, x: f64, y: f64) -> f64 {
        let xi = x.floor() as i64;
        let yi = y.floor() as i64;
        let mut best = f64::MAX;
        for oy in -1..=1 {
            for ox in -1..=1 {
                let (px, py) = self.feature_point(xi + ox, yi + oy);
                let d = (px - x).powi(2) + (py - y).powi(2);
                if d < best {
                    best = d;
                }
            }
        }
        (best.sqrt().min(1.0)) * 2.0 - 1.0
    }

    fn octave(&self, kind: NoiseKind, x: f64, y: f64) -> f64 {
        match kind {
            NoiseKind::Perlin => self.perlin(x, y),
            NoiseKind::Value => self.value(x, y),
            NoiseKind::Worley => self.worley(x, y),
            NoiseKind::Ridged => 1.0 - 2.0 * self.perlin(x, y).abs(),
        }
    }

    fn fbm(&self, x: f64, y: f64) -> f64 {
        let p = self.params.borrow();
        let scale = p.scale.max(0.001);
        let octaves = p.octaves.clamp(1, 12);
        let persistence = p.persistence;
        let lacunarity = p.lacunarity;
        let kind = p.kind;
        drop(p);
        let mut total = 0.0;
        let mut amp = 1.0;
        let mut freq = 1.0 / scale;
        let mut max = 0.0;
        for _ in 0..octaves {
            total += amp * self.octave(kind, x * freq + 31.7, y * freq + 17.3);
            max += amp;
            amp *= persistence;
            freq *= lacunarity;
        }
        ((total / max.max(1e-9)) + 1.0) / 2.0
    }

    fn sample(&self, x: f64, y: f64) -> f64 {
        let warp = self.warp.borrow().clone();
        let (mut x, mut y) = (x, y);
        if let Some((w, strength)) = warp {
            if !std::ptr::eq(&*w, self) {
                let dx = w.fbm(x + 512.4, y + 118.6) * 2.0 - 1.0;
                let dy = w.fbm(x + 917.2, y + 331.8) * 2.0 - 1.0;
                x += dx * strength;
                y += dy * strength;
            }
        }
        self.fbm(x, y).clamp(0.0, 1.0)
    }
}

pub struct NoiseObj {
    core: Rc<NoiseCore>,
}

fn apply_noise_params(core: &NoiseCore, opts: &Table) -> mlua::Result<()> {
    let mut p = core.params.borrow_mut();
    if let Some(seed) = opts.get::<Option<f64>>("seed")? {
        p.seed = seed.to_bits();
        *core.perm.borrow_mut() = build_perm(p.seed);
    }
    if let Some(s) = opts.get::<Option<f64>>("scale")? {
        p.scale = s.max(0.001);
    }
    if let Some(o) = opts.get::<Option<u32>>("octaves")? {
        p.octaves = o.clamp(1, 12);
    }
    if let Some(pe) = opts.get::<Option<f64>>("persistence")? {
        p.persistence = pe.clamp(0.0, 2.0);
    }
    if let Some(l) = opts.get::<Option<f64>>("lacunarity")? {
        p.lacunarity = l.clamp(1.0, 8.0);
    }
    if let Some(k) = opts.get::<Option<String>>("kind")? {
        p.kind = parse_noise_kind(Some(k))?;
    }
    Ok(())
}

struct NoiseFillOpts {
    off_x: f64,
    off_y: f64,
    stops: Option<Vec<GradientStop>>,
    alpha_only: bool,
}

fn parse_noise_fill_opts(opts: Option<&Table>) -> mlua::Result<NoiseFillOpts> {
    let mut out = NoiseFillOpts {
        off_x: 0.0,
        off_y: 0.0,
        stops: None,
        alpha_only: false,
    };
    if let Some(o) = opts {
        if let Some(x) = o.get::<Option<f64>>("x")? {
            out.off_x = x;
        }
        if let Some(y) = o.get::<Option<f64>>("y")? {
            out.off_y = y;
        }
        let stop_value: Value = o.get("stops")?;
        if !stop_value.is_nil() {
            out.stops = Some(parse_stops(&stop_value)?);
        }
        if let Some(a) = o.get::<Option<bool>>("alpha")? {
            out.alpha_only = a;
        }
    }
    Ok(out)
}

fn noise_fill_raw(core: &NoiseCore, img: &mut RgbaImage, opts: &NoiseFillOpts) {
    for (x, y, p) in img.enumerate_pixels_mut() {
        let v = core.sample(x as f64 + opts.off_x, y as f64 + opts.off_y);
        if opts.alpha_only {
            p.0[3] = clamp_channel(v * 255.0);
        } else if let Some(stops) = &opts.stops {
            *p = gradient_color(stops, v as f32);
        } else {
            let g = clamp_channel(v * 255.0);
            *p = Rgba([g, g, g, 255]);
        }
    }
}

async fn run_blocking<T: Send + 'static>(
    f: impl FnOnce() -> mlua::Result<T> + Send + 'static,
) -> mlua::Result<T> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| mlua::Error::external(LehuaError::msg(format!("canvas: join error: {e}"))))?
}

impl UserData for NoiseObj {
    fn add_methods<M: UserDataMethods<Self>>(m: &mut M) {
        m.add_method("sample", |_, this, (x, y): (f64, f64)| {
            Ok(this.core.sample(x, y))
        });

        m.add_function("set", |_, (ud, opts): (AnyUserData, Table)| {
            {
                let this = ud
                    .borrow::<NoiseObj>()
                    .map_err(|_| LehuaError::msg("expected a noise map"))?;
                apply_noise_params(&this.core, &opts)?;
            }
            Ok(ud)
        });

        m.add_function(
            "warp",
            |_, (ud, other, strength): (AnyUserData, AnyUserData, f64)| {
                {
                    let this = ud
                        .borrow::<NoiseObj>()
                        .map_err(|_| LehuaError::msg("expected a noise map"))?;
                    let other = other
                        .borrow::<NoiseObj>()
                        .map_err(|_| LehuaError::msg("warp expects another noise map"))?;
                    if std::ptr::eq(&*this.core, &*other.core) {
                        return Err(LehuaError::msg("a noise map cannot warp itself").into());
                    }
                    let mut cur = Some(other.core.clone());
                    while let Some(c) = cur {
                        if std::ptr::eq(&*c, &*this.core) {
                            return Err(LehuaError::msg(
                                "warp would create a cycle between noise maps",
                            )
                            .into());
                        }
                        let next = c.warp.borrow().as_ref().map(|(n, _)| n.clone());
                        cur = next;
                    }
                    *this.core.warp.borrow_mut() = Some((other.core.clone(), strength));
                }
                Ok(ud)
            },
        );

        m.add_function("unwarp", |_, ud: AnyUserData| {
            {
                let this = ud
                    .borrow::<NoiseObj>()
                    .map_err(|_| LehuaError::msg("expected a noise map"))?;
                *this.core.warp.borrow_mut() = None;
            }
            Ok(ud)
        });

        m.add_async_method(
            "fill",
            |_, this, (target, opts): (AnyUserData, Option<Table>)| {
                let spec = snapshot_core(&this.core);
                let parsed = parse_noise_fill_opts(opts.as_ref());
                async move {
                    let parsed = parsed?;
                    let mut img = {
                        let target = target
                            .borrow::<Canvas>()
                            .map_err(|_| LehuaError::msg("fill expects a canvas"))?;
                        let img = target.img.borrow().clone();
                        img
                    };
                    let out = run_blocking(move || {
                        let core = rebuild_core(spec);
                        noise_fill_raw(&core, &mut img, &parsed);
                        Ok(img)
                    })
                    .await?;
                    let target = target
                        .borrow::<Canvas>()
                        .map_err(|_| LehuaError::msg("fill expects a canvas"))?;
                    *target.img.borrow_mut() = out;
                    Ok(())
                }
            },
        );

        m.add_async_method(
            "image",
            |_, this, (w, h, opts): (u32, u32, Option<Table>)| {
                let spec = snapshot_core(&this.core);
                let parsed = parse_noise_fill_opts(opts.as_ref());
                async move {
                    if w == 0 || h == 0 || w > 16384 || h > 16384 {
                        return Err(
                            LehuaError::msg("image: size must be between 1x1 and 16384x16384")
                                .into(),
                        );
                    }
                    let parsed = parsed?;
                    let img = run_blocking(move || {
                        let core = rebuild_core(spec);
                        let mut img = RgbaImage::new(w, h);
                        noise_fill_raw(&core, &mut img, &parsed);
                        Ok(img)
                    })
                    .await?;
                    Ok(from_image(img))
                }
            },
        );

        m.add_meta_method(MetaMethod::ToString, |_, this, ()| {
            let p = this.core.params.borrow();
            let kind = match p.kind {
                NoiseKind::Perlin => "perlin",
                NoiseKind::Value => "value",
                NoiseKind::Worley => "worley",
                NoiseKind::Ridged => "ridged",
            };
            Ok(format!("NoiseMap({kind}, scale {})", p.scale))
        });
    }
}

impl UserData for FontObj {
    fn add_methods<M: UserDataMethods<Self>>(m: &mut M) {
        m.add_method("measure", |_, this, (text, size): (String, f32)| {
            let scale = PxScale::from(size);
            let mut max_w = 0u32;
            let mut total_h = 0u32;
            for line in text.split('\n') {
                let (w, h) = drawing::text_size(scale, &this.font, line);
                max_w = max_w.max(w);
                total_h += h.max(size as u32);
            }
            Ok((max_w, total_h))
        });

        m.add_meta_method(MetaMethod::ToString, |_, _, ()| Ok("Font"));
    }
}

impl UserData for Canvas {
    fn add_methods<M: UserDataMethods<Self>>(m: &mut M) {
        m.add_method("width", |_, this, ()| Ok(this.img.borrow().width()));
        m.add_method("height", |_, this, ()| Ok(this.img.borrow().height()));
        m.add_method("size", |_, this, ()| {
            let img = this.img.borrow();
            Ok((img.width(), img.height()))
        });

        m.add_method("clone", |_, this, ()| {
            Ok(from_image(this.img.borrow().clone()))
        });

        m.add_method("getPixel", |lua, this, (x, y): (u32, u32)| {
            let img = this.img.borrow();
            if x >= img.width() || y >= img.height() {
                return Err(LehuaError::msg(format!(
                    "pixel ({x}, {y}) is outside the {}x{} canvas",
                    img.width(),
                    img.height()
                ))
                .into());
            }
            color_to_table(lua, *img.get_pixel(x, y))
        });

        m.add_function("setPixel", |_, (ud, x, y, color): (AnyUserData, i64, i64, Value)| {
            {
                let c = parse_color(&color)?;
                let this = borrow_canvas(&ud)?;
                let mut img = this.img.borrow_mut();
                if x >= 0 && y >= 0 && (x as u32) < img.width() && (y as u32) < img.height() {
                    img.put_pixel(x as u32, y as u32, c);
                }
            }
            Ok(ud)
        });

        m.add_method("pixels", |lua, this, ()| {
            lua.create_string(this.img.borrow().as_raw())
        });

        m.add_function("mapPixels", |_, (ud, f): (AnyUserData, Function)| {
            let (w, h, mut data) = {
                let this = borrow_canvas(&ud)?;
                let img = this.img.borrow();
                (img.width(), img.height(), img.as_raw().clone())
            };
            for y in 0..h {
                for x in 0..w {
                    let i = ((y * w + x) * 4) as usize;
                    let (r, g, b, a): (Option<f64>, Option<f64>, Option<f64>, Option<f64>) = f.call((
                        x,
                        y,
                        data[i],
                        data[i + 1],
                        data[i + 2],
                        data[i + 3],
                    ))?;
                    if let Some(r) = r {
                        data[i] = clamp_channel(r);
                    }
                    if let Some(g) = g {
                        data[i + 1] = clamp_channel(g);
                    }
                    if let Some(b) = b {
                        data[i + 2] = clamp_channel(b);
                    }
                    if let Some(a) = a {
                        data[i + 3] = clamp_channel(a);
                    }
                }
            }
            {
                let this = borrow_canvas(&ud)?;
                let img = RgbaImage::from_raw(w, h, data)
                    .ok_or_else(|| LehuaError::msg("mapPixels: buffer size mismatch"))?;
                *this.img.borrow_mut() = img;
            }
            Ok(ud)
        });

        m.add_function("clear", |_, (ud, color): (AnyUserData, Option<Value>)| {
            {
                let this = borrow_canvas(&ud)?;
                let c = opt_color(&color, Rgba([0, 0, 0, 0]))?;
                for p in this.img.borrow_mut().pixels_mut() {
                    *p = c;
                }
            }
            Ok(ud)
        });

        m.add_function("fill", |_, (ud, color): (AnyUserData, Value)| {
            {
                let this = borrow_canvas(&ud)?;
                let c = parse_color(&color)?;
                for p in this.img.borrow_mut().pixels_mut() {
                    *p = c;
                }
            }
            Ok(ud)
        });

        m.add_function(
            "floodFill",
            |_, (ud, x, y, color, tolerance): (AnyUserData, i64, i64, Value, Option<f64>)| {
                {
                    let replacement = parse_color(&color)?;
                    let this = borrow_canvas(&ud)?;
                    let mut img = this.img.borrow_mut();
                    let (w, h) = img.dimensions();
                    if x < 0 || y < 0 || x as u32 >= w || y as u32 >= h {
                        return Err(LehuaError::msg("floodFill start point is outside the canvas").into());
                    }
                    let target = *img.get_pixel(x as u32, y as u32);
                    let tol = tolerance.unwrap_or(0.0).max(0.0) as i32;
                    let matches = |p: &Rgba<u8>| -> bool {
                        (0..4).all(|i| (p.0[i] as i32 - target.0[i] as i32).abs() <= tol)
                    };
                    if !(matches(&replacement) && tol == 0) {
                        let mut stack = vec![(x as u32, y as u32)];
                        let mut visited = vec![false; (w * h) as usize];
                        while let Some((px, py)) = stack.pop() {
                            let idx = (py * w + px) as usize;
                            if visited[idx] {
                                continue;
                            }
                            visited[idx] = true;
                            if !matches(img.get_pixel(px, py)) {
                                continue;
                            }
                            img.put_pixel(px, py, replacement);
                            if px > 0 {
                                stack.push((px - 1, py));
                            }
                            if py > 0 {
                                stack.push((px, py - 1));
                            }
                            if px + 1 < w {
                                stack.push((px + 1, py));
                            }
                            if py + 1 < h {
                                stack.push((px, py + 1));
                            }
                        }
                    }
                }
                Ok(ud)
            },
        );

        m.add_function(
            "line",
            |_, (ud, x1, y1, x2, y2, color, thickness): (AnyUserData, f32, f32, f32, f32, Value, Option<f32>)| {
                {
                    let this = borrow_canvas(&ud)?;
                    let c = parse_color(&color)?;
                    let t = thickness.unwrap_or(1.0);
                    draw_composited(&mut this.img.borrow_mut(), c, |img, c| {
                        thick_line(img, x1, y1, x2, y2, c, t);
                    });
                }
                Ok(ud)
            },
        );

        m.add_function(
            "dashedLine",
            |_, (ud, x1, y1, x2, y2, color, thickness, dash, gap): (AnyUserData, f32, f32, f32, f32, Value, Option<f32>, Option<f32>, Option<f32>)| {
                {
                    let this = borrow_canvas(&ud)?;
                    let c = parse_color(&color)?;
                    let t = thickness.unwrap_or(1.0);
                    let dash = dash.unwrap_or(6.0).max(0.5);
                    let gap = gap.unwrap_or(dash).max(0.5);
                    draw_composited(&mut this.img.borrow_mut(), c, |img, c| {
                        let dx = x2 - x1;
                        let dy = y2 - y1;
                        let len = (dx * dx + dy * dy).sqrt();
                        if len < 0.5 || !len.is_finite() {
                            return;
                        }
                        let ux = dx / len;
                        let uy = dy / len;
                        let step = (dash + gap) as f64;
                        let lenf = len as f64;
                        let diag = (img.width() as f64 + img.height() as f64) * 4.0 + 64.0;
                        let count = (lenf.min(diag) / step).ceil() as u64;
                        for i in 0..count {
                            let start = i as f64 * step;
                            let end = (start + dash as f64).min(lenf);
                            thick_line(
                                img,
                                x1 + ux * start as f32,
                                y1 + uy * start as f32,
                                x1 + ux * end as f32,
                                y1 + uy * end as f32,
                                c,
                                t,
                            );
                        }
                    });
                }
                Ok(ud)
            },
        );

        m.add_function(
            "polyline",
            |_, (ud, points, color, thickness): (AnyUserData, Value, Value, Option<f32>)| {
                {
                    let this = borrow_canvas(&ud)?;
                    let pts = parse_points(&points)?;
                    let c = parse_color(&color)?;
                    let t = thickness.unwrap_or(1.0);
                    draw_composited(&mut this.img.borrow_mut(), c, |img, c| {
                        for pair in pts.windows(2) {
                            thick_line(img, pair[0].0, pair[0].1, pair[1].0, pair[1].1, c, t);
                        }
                    });
                }
                Ok(ud)
            },
        );

        m.add_function(
            "arrow",
            |_, (ud, x1, y1, x2, y2, color, thickness, head_size): (AnyUserData, f32, f32, f32, f32, Value, Option<f32>, Option<f32>)| {
                {
                    let this = borrow_canvas(&ud)?;
                    let c = parse_color(&color)?;
                    let t = thickness.unwrap_or(1.0);
                    draw_composited(&mut this.img.borrow_mut(), c, |img, c| {
                        let dx = x2 - x1;
                        let dy = y2 - y1;
                        let len = (dx * dx + dy * dy).sqrt();
                        if len < 0.5 || !len.is_finite() {
                            return;
                        }
                        let ux = dx / len;
                        let uy = dy / len;
                        let head = head_size.unwrap_or((t * 4.0).max(8.0)).clamp(1.0, len);
                        let bx = x2 - ux * head;
                        let by = y2 - uy * head;
                        thick_line(img, x1, y1, bx, by, c, t);
                        let half = head * 0.5;
                        let px = -uy * half;
                        let py = ux * half;
                        let mut tri = vec![
                            Point::new(x2.round() as i32, y2.round() as i32),
                            Point::new((bx + px).round() as i32, (by + py).round() as i32),
                            Point::new((bx - px).round() as i32, (by - py).round() as i32),
                        ];
                        tri.dedup();
                        if tri.len() >= 3 && tri.first() != tri.last() {
                            drawing::draw_polygon_mut(img, &tri, c);
                        }
                    });
                }
                Ok(ud)
            },
        );

        m.add_function(
            "star",
            |_, (ud, cx, cy, points, outer, inner, color, rotation): (AnyUserData, f32, f32, u32, f32, f32, Value, Option<f32>)| {
                {
                    let this = borrow_canvas(&ud)?;
                    let points = points.clamp(3, 360);
                    let c = parse_color(&color)?;
                    let rot = rotation.unwrap_or(0.0).to_radians() - std::f32::consts::FRAC_PI_2;
                    let mut pts = Vec::with_capacity(points as usize * 2);
                    for i in 0..points * 2 {
                        let r = if i % 2 == 0 { outer } else { inner };
                        let a = rot + std::f32::consts::PI * i as f32 / points as f32;
                        pts.push((cx + r * a.cos(), cy + r * a.sin()));
                    }
                    let poly = polygon_points(&pts)?;
                    draw_composited(&mut this.img.borrow_mut(), c, |img, c| {
                        drawing::draw_polygon_mut(img, &poly, c);
                    });
                }
                Ok(ud)
            },
        );

        m.add_function(
            "grid",
            |_, (ud, spacing_x, spacing_y, color, thickness): (AnyUserData, f32, f32, Value, Option<f32>)| {
                {
                    let this = borrow_canvas(&ud)?;
                    if !(spacing_x.is_finite() && spacing_y.is_finite())
                        || spacing_x < 1.0
                        || spacing_y < 1.0
                    {
                        return Err(LehuaError::msg("grid: spacing must be at least 1").into());
                    }
                    let c = parse_color(&color)?;
                    let t = thickness.unwrap_or(1.0);
                    draw_composited(&mut this.img.borrow_mut(), c, |img, c| {
                        let (w, h) = img.dimensions();
                        let mut x = spacing_x;
                        while x < w as f32 {
                            thick_line(img, x, 0.0, x, h as f32 - 1.0, c, t);
                            x += spacing_x;
                        }
                        let mut y = spacing_y;
                        while y < h as f32 {
                            thick_line(img, 0.0, y, w as f32 - 1.0, y, c, t);
                            y += spacing_y;
                        }
                    });
                }
                Ok(ud)
            },
        );

        m.add_function(
            "rect",
            |_, (ud, x, y, w, h, color): (AnyUserData, i32, i32, u32, u32, Value)| {
                {
                    let this = borrow_canvas(&ud)?;
                    if w > 0 && h > 0 {
                        let c = parse_color(&color)?;
                        draw_composited(&mut this.img.borrow_mut(), c, |img, c| {
                            drawing::draw_filled_rect_mut(img, Rect::at(x, y).of_size(w, h), c);
                        });
                    }
                }
                Ok(ud)
            },
        );

        m.add_function(
            "strokeRect",
            |_, (ud, x, y, w, h, color, thickness): (AnyUserData, f32, f32, f32, f32, Value, Option<f32>)| {
                {
                    let this = borrow_canvas(&ud)?;
                    let c = parse_color(&color)?;
                    let t = thickness.unwrap_or(1.0);
                    draw_composited(&mut this.img.borrow_mut(), c, |img, c| {
                        if t <= 1.5 {
                            drawing::draw_hollow_rect_mut(
                                img,
                                Rect::at(x.round() as i32, y.round() as i32)
                                    .of_size((w.max(1.0)) as u32, (h.max(1.0)) as u32),
                                c,
                            );
                        } else {
                            thick_line(img, x, y, x + w, y, c, t);
                            thick_line(img, x + w, y, x + w, y + h, c, t);
                            thick_line(img, x + w, y + h, x, y + h, c, t);
                            thick_line(img, x, y + h, x, y, c, t);
                        }
                    });
                }
                Ok(ud)
            },
        );

        m.add_function(
            "roundedRect",
            |_, (ud, x, y, w, h, radius, color): (AnyUserData, i32, i32, u32, u32, u32, Value)| {
                {
                    let this = borrow_canvas(&ud)?;
                    let c = parse_color(&color)?;
                    let r = radius.min(w / 2).min(h / 2);
                    draw_composited(&mut this.img.borrow_mut(), c, |img, c| {
                        if r == 0 || w == 0 || h == 0 {
                            if w > 0 && h > 0 {
                                drawing::draw_filled_rect_mut(img, Rect::at(x, y).of_size(w, h), c);
                            }
                        } else {
                            let ri = r as i32;
                            if w > 2 * r {
                                drawing::draw_filled_rect_mut(
                                    img,
                                    Rect::at(x + ri, y).of_size(w - 2 * r, h),
                                    c,
                                );
                            }
                            if h > 2 * r {
                                drawing::draw_filled_rect_mut(
                                    img,
                                    Rect::at(x, y + ri).of_size(w, h - 2 * r),
                                    c,
                                );
                            }
                            let wi = w as i32;
                            let hi = h as i32;
                            for (cx, cy) in [
                                (x + ri, y + ri),
                                (x + wi - ri - 1, y + ri),
                                (x + ri, y + hi - ri - 1),
                                (x + wi - ri - 1, y + hi - ri - 1),
                            ] {
                                drawing::draw_filled_circle_mut(img, (cx, cy), ri, c);
                            }
                        }
                    });
                }
                Ok(ud)
            },
        );

        m.add_function(
            "strokeRoundedRect",
            |_, (ud, x, y, w, h, radius, color, thickness): (AnyUserData, f32, f32, f32, f32, f32, Value, Option<f32>)| {
                {
                    let this = borrow_canvas(&ud)?;
                    let c = parse_color(&color)?;
                    let t = thickness.unwrap_or(1.0);
                    let r = radius.max(0.0).min(w / 2.0).min(h / 2.0);
                    draw_composited(&mut this.img.borrow_mut(), c, |img, c| {
                        thick_line(img, x + r, y, x + w - r, y, c, t);
                        thick_line(img, x + w, y + r, x + w, y + h - r, c, t);
                        thick_line(img, x + w - r, y + h, x + r, y + h, c, t);
                        thick_line(img, x, y + h - r, x, y + r, c, t);
                        if r > 0.5 {
                            stroke_arc(img, x + r, y + r, r, 180.0, 270.0, c, t);
                            stroke_arc(img, x + w - r, y + r, r, 270.0, 360.0, c, t);
                            stroke_arc(img, x + w - r, y + h - r, r, 0.0, 90.0, c, t);
                            stroke_arc(img, x + r, y + h - r, r, 90.0, 180.0, c, t);
                        }
                    });
                }
                Ok(ud)
            },
        );

        m.add_function(
            "circle",
            |_, (ud, cx, cy, r, color): (AnyUserData, i32, i32, i32, Value)| {
                {
                    let this = borrow_canvas(&ud)?;
                    let c = parse_color(&color)?;
                    draw_composited(&mut this.img.borrow_mut(), c, |img, c| {
                        drawing::draw_filled_circle_mut(img, (cx, cy), r.max(0), c);
                    });
                }
                Ok(ud)
            },
        );

        m.add_function(
            "strokeCircle",
            |_, (ud, cx, cy, r, color, thickness): (AnyUserData, i32, i32, i32, Value, Option<f32>)| {
                {
                    let this = borrow_canvas(&ud)?;
                    let c = parse_color(&color)?;
                    let t = thickness.unwrap_or(1.0);
                    draw_composited(&mut this.img.borrow_mut(), c, |img, c| {
                        if t <= 1.5 {
                            drawing::draw_hollow_circle_mut(img, (cx, cy), r.max(0), c);
                        } else {
                            stroke_arc(img, cx as f32, cy as f32, r.max(0) as f32, 0.0, 360.0, c, t);
                        }
                    });
                }
                Ok(ud)
            },
        );

        m.add_function(
            "ellipse",
            |_, (ud, cx, cy, rx, ry, color): (AnyUserData, i32, i32, i32, i32, Value)| {
                {
                    let this = borrow_canvas(&ud)?;
                    let c = parse_color(&color)?;
                    draw_composited(&mut this.img.borrow_mut(), c, |img, c| {
                        drawing::draw_filled_ellipse_mut(img, (cx, cy), rx.max(0), ry.max(0), c);
                    });
                }
                Ok(ud)
            },
        );

        m.add_function(
            "strokeEllipse",
            |_, (ud, cx, cy, rx, ry, color): (AnyUserData, i32, i32, i32, i32, Value)| {
                {
                    let this = borrow_canvas(&ud)?;
                    let c = parse_color(&color)?;
                    draw_composited(&mut this.img.borrow_mut(), c, |img, c| {
                        drawing::draw_hollow_ellipse_mut(img, (cx, cy), rx.max(0), ry.max(0), c);
                    });
                }
                Ok(ud)
            },
        );

        m.add_function(
            "polygon",
            |_, (ud, points, color): (AnyUserData, Value, Value)| {
                {
                    let this = borrow_canvas(&ud)?;
                    let pts = polygon_points(&parse_points(&points)?)?;
                    let c = parse_color(&color)?;
                    draw_composited(&mut this.img.borrow_mut(), c, |img, c| {
                        drawing::draw_polygon_mut(img, &pts, c);
                    });
                }
                Ok(ud)
            },
        );

        m.add_function(
            "strokePolygon",
            |_, (ud, points, color, thickness): (AnyUserData, Value, Value, Option<f32>)| {
                {
                    let this = borrow_canvas(&ud)?;
                    let pts = parse_points(&points)?;
                    if pts.len() < 3 {
                        return Err(LehuaError::msg("a polygon needs at least 3 points").into());
                    }
                    let c = parse_color(&color)?;
                    let t = thickness.unwrap_or(1.0);
                    draw_composited(&mut this.img.borrow_mut(), c, |img, c| {
                        for i in 0..pts.len() {
                            let a = pts[i];
                            let b = pts[(i + 1) % pts.len()];
                            thick_line(img, a.0, a.1, b.0, b.1, c, t);
                        }
                    });
                }
                Ok(ud)
            },
        );

        m.add_function(
            "triangle",
            |_, (ud, x1, y1, x2, y2, x3, y3, color): (AnyUserData, f32, f32, f32, f32, f32, f32, Value)| {
                {
                    let this = borrow_canvas(&ud)?;
                    let pts = polygon_points(&[(x1, y1), (x2, y2), (x3, y3)])?;
                    let c = parse_color(&color)?;
                    draw_composited(&mut this.img.borrow_mut(), c, |img, c| {
                        drawing::draw_polygon_mut(img, &pts, c);
                    });
                }
                Ok(ud)
            },
        );

        m.add_function(
            "arc",
            |_, (ud, cx, cy, r, start, finish, color, thickness): (AnyUserData, f32, f32, f32, f32, f32, Value, Option<f32>)| {
                {
                    let this = borrow_canvas(&ud)?;
                    let c = parse_color(&color)?;
                    let t = thickness.unwrap_or(1.0);
                    draw_composited(&mut this.img.borrow_mut(), c, |img, c| {
                        stroke_arc(img, cx, cy, r, start, finish, c, t);
                    });
                }
                Ok(ud)
            },
        );

        m.add_function(
            "pie",
            |_, (ud, cx, cy, r, start, finish, color): (AnyUserData, f32, f32, f32, f32, f32, Value)| {
                {
                    let this = borrow_canvas(&ud)?;
                    let c = parse_color(&color)?;
                    let start_rad = start.to_radians();
                    let sweep = (finish - start).to_radians();
                    let steps = ((r.abs() * sweep.abs()).ceil() as usize).clamp(8, 720);
                    let mut pts = vec![(cx, cy)];
                    for i in 0..=steps {
                        let a = start_rad + sweep * i as f32 / steps as f32;
                        pts.push((cx + r * a.cos(), cy + r * a.sin()));
                    }
                    let poly = match polygon_points(&pts) {
                        Ok(p) => p,
                        Err(_) => return Ok(ud),
                    };
                    draw_composited(&mut this.img.borrow_mut(), c, |img, c| {
                        drawing::draw_polygon_mut(img, &poly, c);
                    });
                }
                Ok(ud)
            },
        );

        m.add_function(
            "bezier",
            |_, (ud, x1, y1, cx1, cy1, cx2, cy2, x2, y2, color): (AnyUserData, f32, f32, f32, f32, f32, f32, f32, f32, Value)| {
                {
                    let this = borrow_canvas(&ud)?;
                    let c = parse_color(&color)?;
                    draw_composited(&mut this.img.borrow_mut(), c, |img, c| {
                        drawing::draw_cubic_bezier_curve_mut(
                            img,
                            (x1, y1),
                            (x2, y2),
                            (cx1, cy1),
                            (cx2, cy2),
                            c,
                        );
                    });
                }
                Ok(ud)
            },
        );

        m.add_function(
            "linearGradient",
            |_, (ud, x1, y1, x2, y2, stops): (AnyUserData, f32, f32, f32, f32, Value)| {
                {
                    let this = borrow_canvas(&ud)?;
                    let stops = parse_stops(&stops)?;
                    let mut img = this.img.borrow_mut();
                    let dx = x2 - x1;
                    let dy = y2 - y1;
                    let len_sq = (dx * dx + dy * dy).max(1e-6);
                    for (x, y, p) in img.enumerate_pixels_mut() {
                        let t = ((x as f32 - x1) * dx + (y as f32 - y1) * dy) / len_sq;
                        *p = gradient_color(&stops, t);
                    }
                }
                Ok(ud)
            },
        );

        m.add_function(
            "radialGradient",
            |_, (ud, cx, cy, radius, stops): (AnyUserData, f32, f32, f32, Value)| {
                {
                    let this = borrow_canvas(&ud)?;
                    let stops = parse_stops(&stops)?;
                    let r = radius.max(1e-6);
                    let mut img = this.img.borrow_mut();
                    for (x, y, p) in img.enumerate_pixels_mut() {
                        let dx = x as f32 - cx;
                        let dy = y as f32 - cy;
                        let t = (dx * dx + dy * dy).sqrt() / r;
                        *p = gradient_color(&stops, t);
                    }
                }
                Ok(ud)
            },
        );

        m.add_function(
            "conicGradient",
            |_, (ud, cx, cy, start, stops): (AnyUserData, f32, f32, Option<f32>, Value)| {
                {
                    let this = borrow_canvas(&ud)?;
                    let stops = parse_stops(&stops)?;
                    let offset = start.unwrap_or(0.0).to_radians();
                    let tau = std::f32::consts::TAU;
                    let mut img = this.img.borrow_mut();
                    for (x, y, p) in img.enumerate_pixels_mut() {
                        let angle = (y as f32 - cy).atan2(x as f32 - cx) - offset;
                        let t = ((angle % tau) + tau) % tau / tau;
                        *p = gradient_color(&stops, t);
                    }
                }
                Ok(ud)
            },
        );

        m.add_function(
            "text",
            |_, (ud, text, x, y, size, color, font, opts): (AnyUserData, String, i32, i32, f32, Value, AnyUserData, Option<Table>)| {
                {
                    let this = borrow_canvas(&ud)?;
                    let font = font
                        .borrow::<FontObj>()
                        .map_err(|_| LehuaError::msg("text: expected a font from canvas.font()"))?;
                    let c = parse_color(&color)?;
                    let scale = PxScale::from(size);
                    let mut spacing = 0.0f32;
                    let mut line_height = (size * 1.2).round() as i32;
                    if let Some(o) = &opts {
                        if let Some(s) = o.get::<Option<f32>>("spacing")? {
                            spacing = s;
                        }
                        if let Some(lh) = o.get::<Option<f32>>("lineHeight")? {
                            line_height = lh.round() as i32;
                        }
                    }
                    draw_composited(&mut this.img.borrow_mut(), c, |img, c| {
                        for (i, line) in text.split('\n').enumerate() {
                            draw_text_line(
                                img,
                                c,
                                x,
                                y + line_height * i as i32,
                                scale,
                                &font.font,
                                line,
                                spacing,
                            );
                        }
                    });
                }
                Ok(ud)
            },
        );

        m.add_function(
            "drawImage",
            |_, (ud, other, x, y, opts): (AnyUserData, AnyUserData, i64, i64, Option<Table>)| {
                {
                    let this = borrow_canvas(&ud)?;
                    let src = borrow_canvas(&other)?;
                    let mut opacity = 1.0f64;
                    let mut mode = Blend::Normal;
                    let mut scale_to: Option<(u32, u32)> = None;
                    let mut filter = FilterType::Lanczos3;
                    if let Some(o) = &opts {
                        if let Some(a) = o.get::<Option<f64>>("opacity")? {
                            opacity = a;
                        }
                        mode = parse_blend(o.get::<Option<String>>("blend")?)?;
                        let w = o.get::<Option<u32>>("width")?;
                        let h = o.get::<Option<u32>>("height")?;
                        if w.is_some() || h.is_some() {
                            let sw = src.img.borrow().width();
                            let sh = src.img.borrow().height();
                            let w = w.unwrap_or(sw).max(1);
                            let h = h.unwrap_or(sh).max(1);
                            scale_to = Some((w, h));
                        }
                        filter = filter_name(o.get::<Option<String>>("filter")?)?;
                    }
                    if std::ptr::eq(&*this.img.borrow(), &*src.img.borrow()) {
                        return Err(LehuaError::msg("drawImage: cannot draw a canvas onto itself; clone it first").into());
                    }
                    let src_borrow = src.img.borrow();
                    let scaled;
                    let src_img: &RgbaImage = match scale_to {
                        Some((w, h)) if (w, h) != src_borrow.dimensions() => {
                            scaled = imageops::resize(&*src_borrow, w, h, filter);
                            &scaled
                        }
                        _ => &src_borrow,
                    };
                    let mut dst = this.img.borrow_mut();
                    let (dw, dh) = dst.dimensions();
                    for (sx, sy, sp) in src_img.enumerate_pixels() {
                        let dx = x + sx as i64;
                        let dy = y + sy as i64;
                        if dx < 0 || dy < 0 || dx as u32 >= dw || dy as u32 >= dh {
                            continue;
                        }
                        let dp = *dst.get_pixel(dx as u32, dy as u32);
                        dst.put_pixel(dx as u32, dy as u32, composite_pixel(dp, *sp, mode, opacity));
                    }
                }
                Ok(ud)
            },
        );

        m.add_function(
            "merge",
            |_, (ud, other, mode, opacity): (AnyUserData, AnyUserData, Option<String>, Option<f64>)| {
                {
                    let this = borrow_canvas(&ud)?;
                    let src = borrow_canvas(&other)?;
                    if std::ptr::eq(&*this.img.borrow(), &*src.img.borrow()) {
                        return Err(LehuaError::msg("merge: cannot merge a canvas into itself; clone it first").into());
                    }
                    let blend = parse_blend(mode)?;
                    let alpha = opacity.unwrap_or(1.0);
                    let src_img = src.img.borrow();
                    let mut dst = this.img.borrow_mut();
                    let w = dst.width().min(src_img.width());
                    let h = dst.height().min(src_img.height());
                    for y in 0..h {
                        for x in 0..w {
                            let dp = *dst.get_pixel(x, y);
                            let sp = *src_img.get_pixel(x, y);
                            dst.put_pixel(x, y, composite_pixel(dp, sp, blend, alpha));
                        }
                    }
                }
                Ok(ud)
            },
        );

        m.add_function("tile", |_, (ud, other): (AnyUserData, AnyUserData)| {
            {
                let this = borrow_canvas(&ud)?;
                let src = borrow_canvas(&other)?;
                if std::ptr::eq(&*this.img.borrow(), &*src.img.borrow()) {
                    return Err(LehuaError::msg("tile: cannot tile a canvas onto itself; clone it first").into());
                }
                let src_img = src.img.borrow();
                imageops::tile(&mut *this.img.borrow_mut(), &*src_img);
            }
            Ok(ud)
        });

        m.add_function(
            "mask",
            |_, (ud, other, mode): (AnyUserData, AnyUserData, Option<String>)| {
                {
                    let this = borrow_canvas(&ud)?;
                    let src = borrow_canvas(&other)?;
                    let use_alpha = match mode.as_deref().unwrap_or("luminance") {
                        "luminance" | "luma" => false,
                        "alpha" => true,
                        other => {
                            return Err(LehuaError::msg(format!(
                                "unknown mask mode '{other}' (luminance or alpha)"
                            ))
                            .into())
                        }
                    };
                    let (mw, mh, factors) = {
                        let mask_img = src.img.borrow();
                        let factors: Vec<f64> = mask_img
                            .pixels()
                            .map(|mp| {
                                if use_alpha {
                                    mp.0[3] as f64 / 255.0
                                } else {
                                    luminance(mp) / 255.0 * (mp.0[3] as f64 / 255.0)
                                }
                            })
                            .collect();
                        (mask_img.width(), mask_img.height(), factors)
                    };
                    let mut dst = this.img.borrow_mut();
                    let w = dst.width().min(mw);
                    let h = dst.height().min(mh);
                    for y in 0..h {
                        for x in 0..w {
                            let factor = factors[(y * mw + x) as usize];
                            let p = dst.get_pixel_mut(x, y);
                            p.0[3] = clamp_channel(p.0[3] as f64 * factor);
                        }
                    }
                }
                Ok(ud)
            },
        );

        m.add_async_function(
            "dropShadow",
            |_, (ud, dx, dy, sigma, color, strength): (AnyUserData, i64, i64, f32, Option<Value>, Option<f64>)| async move {
                let (src, c) = {
                    let this = borrow_canvas(&ud)?;
                    let c = opt_color(&color, Rgba([0, 0, 0, 255]))?;
                    let src = this.img.borrow().clone();
                    (src, c)
                };
                let out = run_blocking(move || {
                    let shadow = shadow_layer(&src, dx, dy, sigma.max(0.0), c, strength.unwrap_or(1.0));
                    let mut out = src;
                    under_composite(&mut out, shadow);
                    Ok(out)
                })
                .await?;
                *borrow_canvas(&ud)?.img.borrow_mut() = out;
                Ok(ud)
            },
        );

        m.add_async_function(
            "glow",
            |_, (ud, sigma, color, strength): (AnyUserData, f32, Option<Value>, Option<f64>)| async move {
                let (src, c) = {
                    let this = borrow_canvas(&ud)?;
                    let c = opt_color(&color, Rgba([255, 255, 255, 255]))?;
                    let src = this.img.borrow().clone();
                    (src, c)
                };
                let out = run_blocking(move || {
                    let shadow = shadow_layer(&src, 0, 0, sigma.max(0.5), c, strength.unwrap_or(2.0));
                    let mut out = src;
                    under_composite(&mut out, shadow);
                    Ok(out)
                })
                .await?;
                *borrow_canvas(&ud)?.img.borrow_mut() = out;
                Ok(ud)
            },
        );

        m.add_async_function(
            "outline",
            |_, (ud, color, radius): (AnyUserData, Value, Option<u32>)| async move {
                let (src, c) = {
                    let this = borrow_canvas(&ud)?;
                    let c = parse_color(&color)?;
                    let src = this.img.borrow().clone();
                    (src, c)
                };
                let radius = radius.unwrap_or(1).clamp(1, 16);
                let out = run_blocking(move || {
                    let dilated = dilate_alpha(&src, radius);
                    let (w, h) = src.dimensions();
                    let mut under = RgbaImage::new(w, h);
                    for (x, y, p) in under.enumerate_pixels_mut() {
                        let a = dilated[(y * w + x) as usize] as u32 * c.0[3] as u32 / 255;
                        *p = Rgba([c.0[0], c.0[1], c.0[2], a as u8]);
                    }
                    let mut out = src;
                    under_composite(&mut out, under);
                    Ok(out)
                })
                .await?;
                *borrow_canvas(&ud)?.img.borrow_mut() = out;
                Ok(ud)
            },
        );

        m.add_async_function(
            "resize",
            |_, (ud, w, h, filter): (AnyUserData, u32, u32, Option<String>)| async move {
                let (src, f) = {
                    let this = borrow_canvas(&ud)?;
                    let f = filter_name(filter)?;
                    if w == 0 || h == 0 {
                        return Err(LehuaError::msg("resize: width and height must be at least 1").into());
                    }
                    let src = this.img.borrow().clone();
                    (src, f)
                };
                let resized = run_blocking(move || Ok(imageops::resize(&src, w, h, f))).await?;
                *borrow_canvas(&ud)?.img.borrow_mut() = resized;
                Ok(ud)
            },
        );

        m.add_async_function(
            "scale",
            |_, (ud, factor, filter): (AnyUserData, f64, Option<String>)| async move {
                let (src, f, nw, nh) = {
                    let this = borrow_canvas(&ud)?;
                    if factor <= 0.0 {
                        return Err(LehuaError::msg("scale: factor must be positive").into());
                    }
                    let f = filter_name(filter)?;
                    let (w, h) = this.img.borrow().dimensions();
                    let nw = ((w as f64 * factor).round() as u32).max(1);
                    let nh = ((h as f64 * factor).round() as u32).max(1);
                    let src = this.img.borrow().clone();
                    (src, f, nw, nh)
                };
                let resized = run_blocking(move || Ok(imageops::resize(&src, nw, nh, f))).await?;
                *borrow_canvas(&ud)?.img.borrow_mut() = resized;
                Ok(ud)
            },
        );

        m.add_async_function("thumbnail", |_, (ud, w, h): (AnyUserData, u32, u32)| async move {
            let src = borrow_canvas(&ud)?.img.borrow().clone();
            let small =
                run_blocking(move || Ok(imageops::thumbnail(&src, w.max(1), h.max(1)))).await?;
            *borrow_canvas(&ud)?.img.borrow_mut() = small;
            Ok(ud)
        });

        m.add_async_function(
            "fit",
            |_, (ud, w, h, background, filter): (AnyUserData, u32, u32, Option<Value>, Option<String>)| async move {
                let (src, f, bg) = {
                    let this = borrow_canvas(&ud)?;
                    if w == 0 || h == 0 || w > 16384 || h > 16384 {
                        return Err(
                            LehuaError::msg("fit: size must be between 1x1 and 16384x16384").into(),
                        );
                    }
                    let f = filter_name(filter)?;
                    let bg = opt_color(&background, Rgba([0, 0, 0, 0]))?;
                    let src = this.img.borrow().clone();
                    (src, f, bg)
                };
                let out = run_blocking(move || {
                    let (sw, sh) = src.dimensions();
                    let ratio = (w as f64 / sw as f64).min(h as f64 / sh as f64);
                    let nw = ((sw as f64 * ratio).round() as u32).clamp(1, w);
                    let nh = ((sh as f64 * ratio).round() as u32).clamp(1, h);
                    let resized = imageops::resize(&src, nw, nh, f);
                    let mut out = RgbaImage::from_pixel(w, h, bg);
                    let ox = (w - nw) / 2;
                    let oy = (h - nh) / 2;
                    for (x, y, sp) in resized.enumerate_pixels() {
                        let dp = *out.get_pixel(ox + x, oy + y);
                        out.put_pixel(ox + x, oy + y, composite_pixel(dp, *sp, Blend::Normal, 1.0));
                    }
                    Ok(out)
                })
                .await?;
                *borrow_canvas(&ud)?.img.borrow_mut() = out;
                Ok(ud)
            },
        );

        m.add_async_function(
            "cover",
            |_, (ud, w, h, filter): (AnyUserData, u32, u32, Option<String>)| async move {
                let (src, f) = {
                    let this = borrow_canvas(&ud)?;
                    if w == 0 || h == 0 || w > 16384 || h > 16384 {
                        return Err(
                            LehuaError::msg("cover: size must be between 1x1 and 16384x16384").into(),
                        );
                    }
                    let f = filter_name(filter)?;
                    let src = this.img.borrow().clone();
                    (src, f)
                };
                let out = run_blocking(move || {
                    let (sw, sh) = src.dimensions();
                    let ratio = (w as f64 / sw as f64).max(h as f64 / sh as f64);
                    let nw = ((sw as f64 * ratio).round() as u32).max(w);
                    let nh = ((sh as f64 * ratio).round() as u32).max(h);
                    let resized = imageops::resize(&src, nw, nh, f);
                    let ox = (nw - w) / 2;
                    let oy = (nh - h) / 2;
                    Ok(imageops::crop_imm(&resized, ox, oy, w, h).to_image())
                })
                .await?;
                *borrow_canvas(&ud)?.img.borrow_mut() = out;
                Ok(ud)
            },
        );

        m.add_function(
            "crop",
            |_, (ud, x, y, w, h): (AnyUserData, u32, u32, u32, u32)| {
                {
                    let this = borrow_canvas(&ud)?;
                    let img = this.img.borrow().clone();
                    let (iw, ih) = img.dimensions();
                    if x >= iw || y >= ih {
                        return Err(LehuaError::msg("crop: origin is outside the canvas").into());
                    }
                    let w = w.min(iw - x).max(1);
                    let h = h.min(ih - y).max(1);
                    let cropped = imageops::crop_imm(&img, x, y, w, h).to_image();
                    *this.img.borrow_mut() = cropped;
                }
                Ok(ud)
            },
        );

        m.add_function("rotate90", |_, ud: AnyUserData| {
            {
                let this = borrow_canvas(&ud)?;
                let rotated = imageops::rotate90(&*this.img.borrow());
                *this.img.borrow_mut() = rotated;
            }
            Ok(ud)
        });

        m.add_function("rotate180", |_, ud: AnyUserData| {
            {
                let this = borrow_canvas(&ud)?;
                let rotated = imageops::rotate180(&*this.img.borrow());
                *this.img.borrow_mut() = rotated;
            }
            Ok(ud)
        });

        m.add_function("rotate270", |_, ud: AnyUserData| {
            {
                let this = borrow_canvas(&ud)?;
                let rotated = imageops::rotate270(&*this.img.borrow());
                *this.img.borrow_mut() = rotated;
            }
            Ok(ud)
        });

        m.add_async_function(
            "rotate",
            |_, (ud, degrees, background): (AnyUserData, f32, Option<Value>)| async move {
                let (src, bg) = {
                    let this = borrow_canvas(&ud)?;
                    let bg = opt_color(&background, Rgba([0, 0, 0, 0]))?;
                    let src = this.img.borrow().clone();
                    (src, bg)
                };
                let rotated = run_blocking(move || {
                    Ok(rotate_about_center(
                        &src,
                        degrees.to_radians(),
                        Interpolation::Bilinear,
                        bg,
                    ))
                })
                .await?;
                *borrow_canvas(&ud)?.img.borrow_mut() = rotated;
                Ok(ud)
            },
        );

        m.add_function("flipX", |_, ud: AnyUserData| {
            {
                let this = borrow_canvas(&ud)?;
                imageops::flip_horizontal_in_place(&mut *this.img.borrow_mut());
            }
            Ok(ud)
        });

        m.add_function("flipY", |_, ud: AnyUserData| {
            {
                let this = borrow_canvas(&ud)?;
                imageops::flip_vertical_in_place(&mut *this.img.borrow_mut());
            }
            Ok(ud)
        });

        m.add_function("shift", |_, (ud, dx, dy): (AnyUserData, i32, i32)| {
            {
                let this = borrow_canvas(&ud)?;
                let shifted = translate(&*this.img.borrow(), (dx, dy));
                *this.img.borrow_mut() = shifted;
            }
            Ok(ud)
        });

        m.add_function(
            "pad",
            |_, (ud, left, top, right, bottom, color): (AnyUserData, u32, u32, u32, u32, Option<Value>)| {
                {
                    let this = borrow_canvas(&ud)?;
                    let bg = opt_color(&color, Rgba([0, 0, 0, 0]))?;
                    let img = this.img.borrow().clone();
                    let (w, h) = img.dimensions();
                    let mut out = RgbaImage::from_pixel(w + left + right, h + top + bottom, bg);
                    imageops::replace(&mut out, &img, left as i64, top as i64);
                    *this.img.borrow_mut() = out;
                }
                Ok(ud)
            },
        );

        m.add_function("trim", |_, ud: AnyUserData| {
            {
                let this = borrow_canvas(&ud)?;
                let img = this.img.borrow().clone();
                let (w, h) = img.dimensions();
                let mut min_x = w;
                let mut min_y = h;
                let mut max_x = 0u32;
                let mut max_y = 0u32;
                let mut found = false;
                for (x, y, p) in img.enumerate_pixels() {
                    if p.0[3] > 0 {
                        found = true;
                        min_x = min_x.min(x);
                        min_y = min_y.min(y);
                        max_x = max_x.max(x);
                        max_y = max_y.max(y);
                    }
                }
                if found && (min_x > 0 || min_y > 0 || max_x < w - 1 || max_y < h - 1) {
                    let cropped =
                        imageops::crop_imm(&img, min_x, min_y, max_x - min_x + 1, max_y - min_y + 1)
                            .to_image();
                    *this.img.borrow_mut() = cropped;
                }
            }
            Ok(ud)
        });

        m.add_function("grayscale", |_, ud: AnyUserData| {
            {
                let this = borrow_canvas(&ud)?;
                for p in this.img.borrow_mut().pixels_mut() {
                    let l = clamp_channel(luminance(p));
                    p.0[0] = l;
                    p.0[1] = l;
                    p.0[2] = l;
                }
            }
            Ok(ud)
        });

        m.add_function("invert", |_, ud: AnyUserData| {
            {
                let this = borrow_canvas(&ud)?;
                for p in this.img.borrow_mut().pixels_mut() {
                    p.0[0] = 255 - p.0[0];
                    p.0[1] = 255 - p.0[1];
                    p.0[2] = 255 - p.0[2];
                }
            }
            Ok(ud)
        });

        m.add_function("sepia", |_, ud: AnyUserData| {
            {
                let this = borrow_canvas(&ud)?;
                for p in this.img.borrow_mut().pixels_mut() {
                    let r = p.0[0] as f64;
                    let g = p.0[1] as f64;
                    let b = p.0[2] as f64;
                    p.0[0] = clamp_channel(0.393 * r + 0.769 * g + 0.189 * b);
                    p.0[1] = clamp_channel(0.349 * r + 0.686 * g + 0.168 * b);
                    p.0[2] = clamp_channel(0.272 * r + 0.534 * g + 0.131 * b);
                }
            }
            Ok(ud)
        });

        m.add_function("brightness", |_, (ud, delta): (AnyUserData, f64)| {
            {
                let this = borrow_canvas(&ud)?;
                for p in this.img.borrow_mut().pixels_mut() {
                    for i in 0..3 {
                        p.0[i] = clamp_channel(p.0[i] as f64 + delta);
                    }
                }
            }
            Ok(ud)
        });

        m.add_function("contrast", |_, (ud, factor): (AnyUserData, f64)| {
            {
                let this = borrow_canvas(&ud)?;
                for p in this.img.borrow_mut().pixels_mut() {
                    for i in 0..3 {
                        p.0[i] = clamp_channel((p.0[i] as f64 - 128.0) * factor + 128.0);
                    }
                }
            }
            Ok(ud)
        });

        m.add_function("saturate", |_, (ud, factor): (AnyUserData, f64)| {
            {
                let this = borrow_canvas(&ud)?;
                for p in this.img.borrow_mut().pixels_mut() {
                    let l = luminance(p);
                    for i in 0..3 {
                        p.0[i] = clamp_channel(l + (p.0[i] as f64 - l) * factor);
                    }
                }
            }
            Ok(ud)
        });

        m.add_function("hueRotate", |_, (ud, degrees): (AnyUserData, i32)| {
            {
                let this = borrow_canvas(&ud)?;
                let rotated = imageops::huerotate(&*this.img.borrow(), degrees);
                *this.img.borrow_mut() = rotated;
            }
            Ok(ud)
        });

        m.add_function(
            "tint",
            |_, (ud, color, strength): (AnyUserData, Value, Option<f64>)| {
                {
                    let this = borrow_canvas(&ud)?;
                    let c = parse_color(&color)?;
                    let s = strength.unwrap_or(0.5).clamp(0.0, 1.0);
                    for p in this.img.borrow_mut().pixels_mut() {
                        for i in 0..3 {
                            p.0[i] = clamp_channel(p.0[i] as f64 * (1.0 - s) + c.0[i] as f64 * s);
                        }
                    }
                }
                Ok(ud)
            },
        );

        m.add_function("opacity", |_, (ud, factor): (AnyUserData, f64)| {
            {
                let this = borrow_canvas(&ud)?;
                let f = factor.clamp(0.0, 1.0);
                for p in this.img.borrow_mut().pixels_mut() {
                    p.0[3] = clamp_channel(p.0[3] as f64 * f);
                }
            }
            Ok(ud)
        });

        m.add_async_function("blur", |_, (ud, sigma): (AnyUserData, f32)| async move {
            let src = borrow_canvas(&ud)?.img.borrow().clone();
            let blurred = run_blocking(move || {
                Ok(imageproc::filter::gaussian_blur_f32(&src, sigma.max(0.01)))
            })
            .await?;
            *borrow_canvas(&ud)?.img.borrow_mut() = blurred;
            Ok(ud)
        });

        m.add_async_function(
            "sharpen",
            |_, (ud, sigma, threshold): (AnyUserData, Option<f32>, Option<i32>)| async move {
                let src = borrow_canvas(&ud)?.img.borrow().clone();
                let sharpened = run_blocking(move || {
                    Ok(imageops::unsharpen(
                        &src,
                        sigma.unwrap_or(1.0).max(0.01),
                        threshold.unwrap_or(0),
                    ))
                })
                .await?;
                *borrow_canvas(&ud)?.img.borrow_mut() = sharpened;
                Ok(ud)
            },
        );

        m.add_function("pixelate", |_, (ud, block): (AnyUserData, u32)| {
            {
                let this = borrow_canvas(&ud)?;
                let block = block.max(1);
                let mut img = this.img.borrow_mut();
                let (w, h) = img.dimensions();
                for by in (0..h).step_by(block as usize) {
                    for bx in (0..w).step_by(block as usize) {
                        let bw = block.min(w - bx);
                        let bh = block.min(h - by);
                        let mut acc = [0u64; 4];
                        for y in by..by + bh {
                            for x in bx..bx + bw {
                                let p = img.get_pixel(x, y);
                                for i in 0..4 {
                                    acc[i] += p.0[i] as u64;
                                }
                            }
                        }
                        let count = (bw * bh) as u64;
                        let avg = Rgba([
                            (acc[0] / count) as u8,
                            (acc[1] / count) as u8,
                            (acc[2] / count) as u8,
                            (acc[3] / count) as u8,
                        ]);
                        for y in by..by + bh {
                            for x in bx..bx + bw {
                                img.put_pixel(x, y, avg);
                            }
                        }
                    }
                }
            }
            Ok(ud)
        });

        m.add_function("posterize", |_, (ud, levels): (AnyUserData, u32)| {
            {
                let this = borrow_canvas(&ud)?;
                let levels = levels.clamp(2, 256) as f64;
                let step = 255.0 / (levels - 1.0);
                for p in this.img.borrow_mut().pixels_mut() {
                    for i in 0..3 {
                        p.0[i] = clamp_channel((p.0[i] as f64 / step).round() * step);
                    }
                }
            }
            Ok(ud)
        });

        m.add_function("threshold", |_, (ud, level): (AnyUserData, Option<f64>)| {
            {
                let this = borrow_canvas(&ud)?;
                let level = level.unwrap_or(128.0);
                for p in this.img.borrow_mut().pixels_mut() {
                    let v = if luminance(p) >= level { 255 } else { 0 };
                    p.0[0] = v;
                    p.0[1] = v;
                    p.0[2] = v;
                }
            }
            Ok(ud)
        });

        m.add_function("noise", |_, (ud, amount): (AnyUserData, f64)| {
            {
                let this = borrow_canvas(&ud)?;
                let mut rng = Lcg::new();
                for p in this.img.borrow_mut().pixels_mut() {
                    let n = rng.next_signed(amount);
                    for i in 0..3 {
                        p.0[i] = clamp_channel(p.0[i] as f64 + n);
                    }
                }
            }
            Ok(ud)
        });

        m.add_function(
            "vignette",
            |_, (ud, strength, color): (AnyUserData, Option<f64>, Option<Value>)| {
                {
                    let this = borrow_canvas(&ud)?;
                    let strength = strength.unwrap_or(0.5).clamp(0.0, 1.0);
                    let c = opt_color(&color, Rgba([0, 0, 0, 255]))?;
                    let mut img = this.img.borrow_mut();
                    let (w, h) = img.dimensions();
                    let cx = w as f64 / 2.0;
                    let cy = h as f64 / 2.0;
                    let max_d = (cx * cx + cy * cy).sqrt();
                    for (x, y, p) in img.enumerate_pixels_mut() {
                        let dx = x as f64 - cx;
                        let dy = y as f64 - cy;
                        let d = (dx * dx + dy * dy).sqrt() / max_d;
                        let f = strength * d * d;
                        for i in 0..3 {
                            p.0[i] = clamp_channel(p.0[i] as f64 * (1.0 - f) + c.0[i] as f64 * f);
                        }
                    }
                }
                Ok(ud)
            },
        );

        m.add_async_function("emboss", |_, ud: AnyUserData| async move {
            let src = borrow_canvas(&ud)?.img.borrow().clone();
            let out = run_blocking(move || {
                let kernel = [-2.0, -1.0, 0.0, -1.0, 1.0, 1.0, 0.0, 1.0, 2.0];
                Ok(convolve3x3(&src, kernel, 0.0))
            })
            .await?;
            *borrow_canvas(&ud)?.img.borrow_mut() = out;
            Ok(ud)
        });

        m.add_async_function("edges", |_, ud: AnyUserData| async move {
            let src = borrow_canvas(&ud)?.img.borrow().clone();
            let out = run_blocking(move || {
                let gx = convolve3x3(&src, [-1.0, 0.0, 1.0, -2.0, 0.0, 2.0, -1.0, 0.0, 1.0], 0.0);
                let gy = convolve3x3(&src, [-1.0, -2.0, -1.0, 0.0, 0.0, 0.0, 1.0, 2.0, 1.0], 0.0);
                let mut out = src;
                for (x, y, p) in out.enumerate_pixels_mut() {
                    let px = gx.get_pixel(x, y);
                    let py = gy.get_pixel(x, y);
                    for i in 0..3 {
                        let v = ((px.0[i] as f64).powi(2) + (py.0[i] as f64).powi(2)).sqrt();
                        p.0[i] = clamp_channel(v);
                    }
                }
                Ok(out)
            })
            .await?;
            *borrow_canvas(&ud)?.img.borrow_mut() = out;
            Ok(ud)
        });

        m.add_async_function(
            "convolve",
            |_, (ud, kernel, bias): (AnyUserData, Vec<f64>, Option<f64>)| async move {
                let (src, k) = {
                    let this = borrow_canvas(&ud)?;
                    if kernel.len() != 9 {
                        return Err(LehuaError::msg("convolve expects a 3x3 kernel of 9 numbers").into());
                    }
                    let mut k = [0.0f64; 9];
                    k.copy_from_slice(&kernel);
                    let src = this.img.borrow().clone();
                    (src, k)
                };
                let out =
                    run_blocking(move || Ok(convolve3x3(&src, k, bias.unwrap_or(0.0)))).await?;
                *borrow_canvas(&ud)?.img.borrow_mut() = out;
                Ok(ud)
            },
        );

        m.add_function("gamma", |_, (ud, g): (AnyUserData, f64)| {
            {
                let this = borrow_canvas(&ud)?;
                if g <= 0.0 {
                    return Err(LehuaError::msg("gamma must be positive").into());
                }
                let inv = 1.0 / g;
                let mut lut = [0u8; 256];
                for (i, v) in lut.iter_mut().enumerate() {
                    *v = clamp_channel((i as f64 / 255.0).powf(inv) * 255.0);
                }
                for p in this.img.borrow_mut().pixels_mut() {
                    for i in 0..3 {
                        p.0[i] = lut[p.0[i] as usize];
                    }
                }
            }
            Ok(ud)
        });

        m.add_function("exposure", |_, (ud, stops): (AnyUserData, f64)| {
            {
                let this = borrow_canvas(&ud)?;
                let factor = 2.0f64.powf(stops);
                for p in this.img.borrow_mut().pixels_mut() {
                    for i in 0..3 {
                        p.0[i] = clamp_channel(p.0[i] as f64 * factor);
                    }
                }
            }
            Ok(ud)
        });

        m.add_function(
            "levels",
            |_, (ud, in_black, in_white, out_black, out_white): (AnyUserData, f64, f64, Option<f64>, Option<f64>)| {
                {
                    let this = borrow_canvas(&ud)?;
                    let out_black = out_black.unwrap_or(0.0);
                    let out_white = out_white.unwrap_or(255.0);
                    let span = (in_white - in_black).max(1e-6);
                    for p in this.img.borrow_mut().pixels_mut() {
                        for i in 0..3 {
                            let t = ((p.0[i] as f64 - in_black) / span).clamp(0.0, 1.0);
                            p.0[i] = clamp_channel(out_black + t * (out_white - out_black));
                        }
                    }
                }
                Ok(ud)
            },
        );

        m.add_function("normalize", |_, ud: AnyUserData| {
            {
                let this = borrow_canvas(&ud)?;
                let mut img = this.img.borrow_mut();
                let mut lo = 255u8;
                let mut hi = 0u8;
                for p in img.pixels() {
                    if p.0[3] == 0 {
                        continue;
                    }
                    for i in 0..3 {
                        lo = lo.min(p.0[i]);
                        hi = hi.max(p.0[i]);
                    }
                }
                if hi > lo {
                    let span = (hi - lo) as f64;
                    for p in img.pixels_mut() {
                        for i in 0..3 {
                            p.0[i] = clamp_channel((p.0[i] as f64 - lo as f64) / span * 255.0);
                        }
                    }
                }
            }
            Ok(ud)
        });

        m.add_function("equalize", |_, ud: AnyUserData| {
            {
                let this = borrow_canvas(&ud)?;
                let mut img = this.img.borrow_mut();
                let mut hist = [0u64; 256];
                let mut count = 0u64;
                for p in img.pixels() {
                    if p.0[3] > 0 {
                        hist[luminance(p) as usize % 256] += 1;
                        count += 1;
                    }
                }
                if count > 0 {
                    let mut cdf = [0f64; 256];
                    let mut acc = 0u64;
                    for i in 0..256 {
                        acc += hist[i];
                        cdf[i] = acc as f64 / count as f64;
                    }
                    for p in img.pixels_mut() {
                        if p.0[3] == 0 {
                            continue;
                        }
                        let l = luminance(p).max(1.0);
                        let target = cdf[l as usize % 256] * 255.0;
                        let ratio = target / l;
                        for i in 0..3 {
                            p.0[i] = clamp_channel(p.0[i] as f64 * ratio);
                        }
                    }
                }
            }
            Ok(ud)
        });

        m.add_function(
            "duotone",
            |_, (ud, dark, light): (AnyUserData, Value, Value)| {
                {
                    let this = borrow_canvas(&ud)?;
                    let dark = parse_color(&dark)?;
                    let light = parse_color(&light)?;
                    for p in this.img.borrow_mut().pixels_mut() {
                        let t = luminance(p) / 255.0;
                        for i in 0..3 {
                            p.0[i] = clamp_channel(
                                dark.0[i] as f64 + (light.0[i] as f64 - dark.0[i] as f64) * t,
                            );
                        }
                    }
                }
                Ok(ud)
            },
        );

        m.add_function("temperature", |_, (ud, amount): (AnyUserData, f64)| {
            {
                let this = borrow_canvas(&ud)?;
                let shift = amount.clamp(-100.0, 100.0) * 0.6;
                for p in this.img.borrow_mut().pixels_mut() {
                    p.0[0] = clamp_channel(p.0[0] as f64 + shift);
                    p.0[2] = clamp_channel(p.0[2] as f64 - shift);
                }
            }
            Ok(ud)
        });

        m.add_function("solarize", |_, (ud, threshold): (AnyUserData, Option<f64>)| {
            {
                let this = borrow_canvas(&ud)?;
                let t = threshold.unwrap_or(128.0);
                for p in this.img.borrow_mut().pixels_mut() {
                    for i in 0..3 {
                        if p.0[i] as f64 >= t {
                            p.0[i] = 255 - p.0[i];
                        }
                    }
                }
            }
            Ok(ud)
        });

        m.add_function(
            "channels",
            |_, (ud, rf, gf, bf): (AnyUserData, f64, f64, f64)| {
                {
                    let this = borrow_canvas(&ud)?;
                    for p in this.img.borrow_mut().pixels_mut() {
                        p.0[0] = clamp_channel(p.0[0] as f64 * rf);
                        p.0[1] = clamp_channel(p.0[1] as f64 * gf);
                        p.0[2] = clamp_channel(p.0[2] as f64 * bf);
                    }
                }
                Ok(ud)
            },
        );

        m.add_function(
            "colorReplace",
            |_, (ud, from, to, tolerance): (AnyUserData, Value, Value, Option<f64>)| {
                {
                    let this = borrow_canvas(&ud)?;
                    let from = parse_color(&from)?;
                    let to = parse_color(&to)?;
                    let tol = tolerance.unwrap_or(0.0).max(0.0) as i32;
                    for p in this.img.borrow_mut().pixels_mut() {
                        let matches =
                            (0..3).all(|i| (p.0[i] as i32 - from.0[i] as i32).abs() <= tol);
                        if matches {
                            p.0[0] = to.0[0];
                            p.0[1] = to.0[1];
                            p.0[2] = to.0[2];
                        }
                    }
                }
                Ok(ud)
            },
        );

        m.add_function(
            "chromaKey",
            |_, (ud, color, tolerance, softness): (AnyUserData, Value, Option<f64>, Option<f64>)| {
                {
                    let this = borrow_canvas(&ud)?;
                    let key = parse_color(&color)?;
                    let tol = tolerance.unwrap_or(60.0).max(0.0);
                    let soft = softness.unwrap_or(20.0).max(0.0);
                    for p in this.img.borrow_mut().pixels_mut() {
                        let dist = (0..3)
                            .map(|i| (p.0[i] as f64 - key.0[i] as f64).powi(2))
                            .sum::<f64>()
                            .sqrt();
                        if dist <= tol {
                            p.0[3] = 0;
                        } else if soft > 0.0 && dist < tol + soft {
                            let keep = (dist - tol) / soft;
                            p.0[3] = clamp_channel(p.0[3] as f64 * keep);
                        }
                    }
                }
                Ok(ud)
            },
        );

        m.add_function("dither", |_, (ud, levels): (AnyUserData, Option<u32>)| {
            {
                let this = borrow_canvas(&ud)?;
                let levels = levels.unwrap_or(2).clamp(2, 256) as f64;
                let step = 255.0 / (levels - 1.0);
                let mut img = this.img.borrow_mut();
                let (w, h) = img.dimensions();
                let mut buf: Vec<[f64; 3]> = img
                    .pixels()
                    .map(|p| [p.0[0] as f64, p.0[1] as f64, p.0[2] as f64])
                    .collect();
                for y in 0..h as usize {
                    for x in 0..w as usize {
                        let idx = y * w as usize + x;
                        for c in 0..3 {
                            let old = buf[idx][c];
                            let new = (old / step).round() * step;
                            let err = old - new;
                            buf[idx][c] = new;
                            if x + 1 < w as usize {
                                buf[idx + 1][c] += err * 7.0 / 16.0;
                            }
                            if y + 1 < h as usize {
                                if x > 0 {
                                    buf[idx + w as usize - 1][c] += err * 3.0 / 16.0;
                                }
                                buf[idx + w as usize][c] += err * 5.0 / 16.0;
                                if x + 1 < w as usize {
                                    buf[idx + w as usize + 1][c] += err * 1.0 / 16.0;
                                }
                            }
                        }
                    }
                }
                for (i, p) in img.pixels_mut().enumerate() {
                    p.0[0] = clamp_channel(buf[i][0]);
                    p.0[1] = clamp_channel(buf[i][1]);
                    p.0[2] = clamp_channel(buf[i][2]);
                }
            }
            Ok(ud)
        });

        m.add_function(
            "scanlines",
            |_, (ud, spacing, opacity): (AnyUserData, Option<u32>, Option<f64>)| {
                {
                    let this = borrow_canvas(&ud)?;
                    let spacing = spacing.unwrap_or(3).max(2);
                    let keep = 1.0 - opacity.unwrap_or(0.3).clamp(0.0, 1.0);
                    for (_, y, p) in this.img.borrow_mut().enumerate_pixels_mut() {
                        if y % spacing == 0 {
                            for i in 0..3 {
                                p.0[i] = clamp_channel(p.0[i] as f64 * keep);
                            }
                        }
                    }
                }
                Ok(ud)
            },
        );

        m.add_function(
            "motionBlur",
            |_, (ud, angle, distance): (AnyUserData, f64, u32)| {
                {
                    let this = borrow_canvas(&ud)?;
                    let distance = distance.clamp(1, 256) as i64;
                    let rad = angle.to_radians();
                    let (dx, dy) = (rad.cos(), rad.sin());
                    let src = this.img.borrow().clone();
                    let (w, h) = src.dimensions();
                    let mut img = this.img.borrow_mut();
                    for (x, y, p) in img.enumerate_pixels_mut() {
                        let mut acc = [0f64; 4];
                        let mut n = 0f64;
                        for i in -distance / 2..=distance / 2 {
                            let sx = x as i64 + (dx * i as f64).round() as i64;
                            let sy = y as i64 + (dy * i as f64).round() as i64;
                            if sx < 0 || sy < 0 || sx as u32 >= w || sy as u32 >= h {
                                continue;
                            }
                            let sp = src.get_pixel(sx as u32, sy as u32);
                            for c in 0..4 {
                                acc[c] += sp.0[c] as f64;
                            }
                            n += 1.0;
                        }
                        if n > 0.0 {
                            for c in 0..4 {
                                p.0[c] = clamp_channel(acc[c] / n);
                            }
                        }
                    }
                }
                Ok(ud)
            },
        );

        m.add_function(
            "wave",
            |_, (ud, amplitude_x, wavelength_x, amplitude_y, wavelength_y): (AnyUserData, f64, f64, Option<f64>, Option<f64>)| {
                {
                    let this = borrow_canvas(&ud)?;
                    let ax = amplitude_x;
                    let wx = wavelength_x.max(1.0);
                    let ay = amplitude_y.unwrap_or(0.0);
                    let wy = wavelength_y.unwrap_or(wx).max(1.0);
                    let tau = std::f64::consts::TAU;
                    displace(&mut this.img.borrow_mut(), |x, y| {
                        (x + ax * (tau * y / wx).sin(), y + ay * (tau * x / wy).sin())
                    });
                }
                Ok(ud)
            },
        );

        m.add_function(
            "swirl",
            |_, (ud, cx, cy, radius, degrees): (AnyUserData, f64, f64, f64, f64)| {
                {
                    let this = borrow_canvas(&ud)?;
                    let radius = radius.max(1.0);
                    let strength = degrees.to_radians();
                    displace(&mut this.img.borrow_mut(), |x, y| {
                        let dx = x - cx;
                        let dy = y - cy;
                        let r = (dx * dx + dy * dy).sqrt();
                        if r >= radius {
                            return (x, y);
                        }
                        let angle = strength * (1.0 - r / radius).powi(2);
                        let (s, c) = angle.sin_cos();
                        (cx + dx * c - dy * s, cy + dx * s + dy * c)
                    });
                }
                Ok(ud)
            },
        );

        m.add_function(
            "bulge",
            |_, (ud, cx, cy, radius, amount): (AnyUserData, f64, f64, f64, f64)| {
                {
                    let this = borrow_canvas(&ud)?;
                    let radius = radius.max(1.0);
                    let amount = amount.clamp(-0.95, 0.95);
                    displace(&mut this.img.borrow_mut(), |x, y| {
                        let dx = x - cx;
                        let dy = y - cy;
                        let r = (dx * dx + dy * dy).sqrt();
                        if r >= radius || r < 1e-6 {
                            return (x, y);
                        }
                        let d = r / radius;
                        let f = d.powf(1.0 + amount);
                        let scale = f * radius / r;
                        (cx + dx * scale, cy + dy * scale)
                    });
                }
                Ok(ud)
            },
        );

        m.add_function(
            "chromaticAberration",
            |_, (ud, offset): (AnyUserData, i64)| {
                {
                    let this = borrow_canvas(&ud)?;
                    let src = this.img.borrow().clone();
                    let (w, h) = src.dimensions();
                    let sample = |x: i64, y: i64, c: usize| -> u8 {
                        if x < 0 || y < 0 || x as u32 >= w || y as u32 >= h {
                            return 0;
                        }
                        src.get_pixel(x as u32, y as u32).0[c]
                    };
                    let mut img = this.img.borrow_mut();
                    for (x, y, p) in img.enumerate_pixels_mut() {
                        p.0[0] = sample(x as i64 - offset, y as i64, 0);
                        p.0[2] = sample(x as i64 + offset, y as i64, 2);
                    }
                }
                Ok(ud)
            },
        );

        m.add_function(
            "skew",
            |_, (ud, x_degrees, y_degrees, background): (AnyUserData, f64, f64, Option<Value>)| {
                {
                    let this = borrow_canvas(&ud)?;
                    let bg = opt_color(&background, Rgba([0, 0, 0, 0]))?;
                    let tx = x_degrees.to_radians().tan();
                    let ty = y_degrees.to_radians().tan();
                    let src = this.img.borrow().clone();
                    let (w, h) = src.dimensions();
                    let mut img = this.img.borrow_mut();
                    for (x, y, p) in img.enumerate_pixels_mut() {
                        let sx = x as f64 - tx * y as f64;
                        let sy = y as f64 - ty * x as f64;
                        let sample = sample_bilinear(&src, sx, sy);
                        *p = if sx < -1.0 || sy < -1.0 || sx > w as f64 || sy > h as f64 {
                            bg
                        } else {
                            sample
                        };
                    }
                }
                Ok(ud)
            },
        );

        m.add_function(
            "blit",
            |_, (ud, other, sx, sy, sw, sh, dx, dy): (AnyUserData, AnyUserData, u32, u32, u32, u32, i64, i64)| {
                {
                    let this = borrow_canvas(&ud)?;
                    let src = other
                        .borrow::<Canvas>()
                        .map_err(|_| LehuaError::msg("blit expects a canvas as its first argument"))?;
                    let region: Vec<(u32, u32, Rgba<u8>)> = {
                        let src_img = src.img.borrow();
                        let (w, h) = src_img.dimensions();
                        if sx >= w || sy >= h {
                            Vec::new()
                        } else {
                            let sw = sw.min(w - sx);
                            let sh = sh.min(h - sy);
                            let mut out = Vec::with_capacity((sw * sh) as usize);
                            for y in 0..sh {
                                for x in 0..sw {
                                    out.push((x, y, *src_img.get_pixel(sx + x, sy + y)));
                                }
                            }
                            out
                        }
                    };
                    let mut dst = this.img.borrow_mut();
                    let (dw, dh) = dst.dimensions();
                    for (x, y, sp) in region {
                        let tx = dx + x as i64;
                        let ty = dy + y as i64;
                        if tx < 0 || ty < 0 || tx as u32 >= dw || ty as u32 >= dh {
                            continue;
                        }
                        let dp = *dst.get_pixel(tx as u32, ty as u32);
                        dst.put_pixel(tx as u32, ty as u32, composite_pixel(dp, sp, Blend::Normal, 1.0));
                    }
                }
                Ok(ud)
            },
        );

        m.add_method("channel", |_, this, name: String| {
            let idx = channel_index(&name)?;
            let img = this.img.borrow();
            let mut out = RgbaImage::new(img.width(), img.height());
            for (x, y, p) in img.enumerate_pixels() {
                let v = p.0[idx];
                out.put_pixel(x, y, Rgba([v, v, v, 255]));
            }
            Ok(from_image(out))
        });

        m.add_function(
            "setChannel",
            |_, (ud, name, source): (AnyUserData, String, AnyUserData)| {
                {
                    let this = borrow_canvas(&ud)?;
                    let idx = channel_index(&name)?;
                    let src = source
                        .borrow::<Canvas>()
                        .map_err(|_| LehuaError::msg("setChannel expects a canvas as its source"))?;
                    let values: Vec<u8> = {
                        let src_img = src.img.borrow();
                        src_img.pixels().map(|p| clamp_channel(luminance(p))).collect()
                    };
                    let (sw, sh) = {
                        let src_img = src.img.borrow();
                        src_img.dimensions()
                    };
                    let mut dst = this.img.borrow_mut();
                    let w = dst.width().min(sw);
                    let h = dst.height().min(sh);
                    for y in 0..h {
                        for x in 0..w {
                            let v = values[(y * sw + x) as usize];
                            dst.get_pixel_mut(x, y).0[idx] = v;
                        }
                    }
                }
                Ok(ud)
            },
        );

        m.add_method("splitChannels", |_, this, ()| {
            let img = this.img.borrow();
            let mut outs: Vec<RgbaImage> = (0..4)
                .map(|_| RgbaImage::new(img.width(), img.height()))
                .collect();
            for (x, y, p) in img.enumerate_pixels() {
                for c in 0..4 {
                    let v = p.0[c];
                    outs[c].put_pixel(x, y, Rgba([v, v, v, 255]));
                }
            }
            let mut it = outs.into_iter();
            Ok((
                from_image(it.next().unwrap()),
                from_image(it.next().unwrap()),
                from_image(it.next().unwrap()),
                from_image(it.next().unwrap()),
            ))
        });

        m.add_method("getBounds", |_, this, ()| {
            let img = this.img.borrow();
            let (w, h) = img.dimensions();
            let mut min_x = w;
            let mut min_y = h;
            let mut max_x = 0u32;
            let mut max_y = 0u32;
            let mut found = false;
            for (x, y, p) in img.enumerate_pixels() {
                if p.0[3] > 0 {
                    found = true;
                    min_x = min_x.min(x);
                    min_y = min_y.min(y);
                    max_x = max_x.max(x);
                    max_y = max_y.max(y);
                }
            }
            if !found {
                return Ok(mlua::MultiValue::from_vec(vec![Value::Nil]));
            }
            Ok(mlua::MultiValue::from_vec(vec![
                Value::Integer(min_x as i64),
                Value::Integer(min_y as i64),
                Value::Integer((max_x - min_x + 1) as i64),
                Value::Integer((max_y - min_y + 1) as i64),
            ]))
        });

        m.add_method("dominantColors", |lua, this, count: Option<usize>| {
            let count = count.unwrap_or(5).clamp(1, 64);
            let img = this.img.borrow();
            let mut buckets: std::collections::HashMap<u32, (u64, [u64; 3])> =
                std::collections::HashMap::new();
            for p in img.pixels() {
                if p.0[3] < 128 {
                    continue;
                }
                let key = ((p.0[0] as u32 >> 4) << 8) | ((p.0[1] as u32 >> 4) << 4) | (p.0[2] as u32 >> 4);
                let entry = buckets.entry(key).or_insert((0, [0; 3]));
                entry.0 += 1;
                for i in 0..3 {
                    entry.1[i] += p.0[i] as u64;
                }
            }
            let mut sorted: Vec<_> = buckets.into_values().collect();
            sorted.sort_by(|a, b| b.0.cmp(&a.0));
            let out = lua.create_table()?;
            for (i, (n, sums)) in sorted.into_iter().take(count).enumerate() {
                let c = color_to_table(
                    lua,
                    Rgba([
                        (sums[0] / n) as u8,
                        (sums[1] / n) as u8,
                        (sums[2] / n) as u8,
                        255,
                    ]),
                )?;
                c.set("count", n)?;
                out.raw_seti(i + 1, c)?;
            }
            Ok(out)
        });

        m.add_method("averageColor", |lua, this, ()| {
            let img = this.img.borrow();
            let mut acc = [0u64; 4];
            let count = (img.width() * img.height()).max(1) as u64;
            for p in img.pixels() {
                for i in 0..4 {
                    acc[i] += p.0[i] as u64;
                }
            }
            color_to_table(
                lua,
                Rgba([
                    (acc[0] / count) as u8,
                    (acc[1] / count) as u8,
                    (acc[2] / count) as u8,
                    (acc[3] / count) as u8,
                ]),
            )
        });

        m.add_method("histogram", |lua, this, channel: Option<String>| {
            let img = this.img.borrow();
            let mut bins = [0u64; 256];
            let channel = channel.unwrap_or_else(|| "luma".to_string());
            for p in img.pixels() {
                let v = match channel.as_str() {
                    "r" | "red" => p.0[0] as usize,
                    "g" | "green" => p.0[1] as usize,
                    "b" | "blue" => p.0[2] as usize,
                    "a" | "alpha" => p.0[3] as usize,
                    _ => luminance(p) as usize,
                };
                bins[v.min(255)] += 1;
            }
            let t = lua.create_table()?;
            for (i, b) in bins.iter().enumerate() {
                t.raw_seti(i + 1, *b)?;
            }
            Ok(t)
        });

        m.add_method("compare", |_, this, other: AnyUserData| {
            let other = borrow_canvas(&other)?;
            let a = this.img.borrow();
            let b = other.img.borrow();
            let w = a.width().min(b.width());
            let h = a.height().min(b.height());
            if w == 0 || h == 0 {
                return Ok(0.0);
            }
            let mut total = 0u64;
            for y in 0..h {
                for x in 0..w {
                    let pa = a.get_pixel(x, y);
                    let pb = b.get_pixel(x, y);
                    for i in 0..4 {
                        total += (pa.0[i] as i64 - pb.0[i] as i64).unsigned_abs();
                    }
                }
            }
            let max = (w as u64) * (h as u64) * 4 * 255;
            Ok(1.0 - total as f64 / max as f64)
        });

        m.add_async_method(
            "encode",
            |lua, this, (format, quality): (Option<String>, Option<u8>)| {
                let fmt = format_from_name(format.as_deref().unwrap_or("png"));
                let img = this.img.borrow().clone();
                async move {
                    let fmt = fmt?;
                    let bytes = run_blocking(move || encode_image(&img, fmt, quality)).await?;
                    lua.create_string(bytes)
                }
            },
        );

        m.add_async_method("dataUrl", |_, this, format: Option<String>| {
            let name = format.unwrap_or_else(|| "png".to_string());
            let fmt = format_from_name(&name);
            let img = this.img.borrow().clone();
            async move {
                use base64::Engine;
                let fmt = fmt?;
                let bytes = run_blocking(move || encode_image(&img, fmt, None)).await?;
                let mime = match fmt {
                    ImageFormat::Jpeg => "image/jpeg",
                    ImageFormat::Gif => "image/gif",
                    ImageFormat::Bmp => "image/bmp",
                    ImageFormat::WebP => "image/webp",
                    _ => "image/png",
                };
                Ok(format!(
                    "data:{mime};base64,{}",
                    base64::engine::general_purpose::STANDARD.encode(bytes)
                ))
            }
        });

        m.add_method(
            "buffer",
            |lua, this, (x, y, w, h): (Option<i64>, Option<i64>, Option<u32>, Option<u32>)| {
                let img = this.img.borrow();
                match (x, y, w, h) {
                    (None, None, None, None) => lua.create_buffer(img.as_raw().as_slice()),
                    (Some(x), Some(y), Some(w), Some(h)) => {
                        let (cw, ch) = img.dimensions();
                        if x < 0
                            || y < 0
                            || w == 0
                            || h == 0
                            || x as u64 + w as u64 > cw as u64
                            || y as u64 + h as u64 > ch as u64
                        {
                            return Err(LehuaError::msg(format!(
                                "buffer: region {w}x{h} at ({x}, {y}) is outside the {cw}x{ch} canvas"
                            ))
                            .into());
                        }
                        let mut out = Vec::with_capacity(w as usize * h as usize * 4);
                        for row in 0..h {
                            for col in 0..w {
                                out.extend_from_slice(&img.get_pixel(x as u32 + col, y as u32 + row).0);
                            }
                        }
                        lua.create_buffer(out)
                    }
                    _ => Err(LehuaError::msg("buffer: pass no region, or all of x, y, w, h").into()),
                }
            },
        );

        m.add_function(
            "setBuffer",
            |_, (ud, data, x, y, w, h): (AnyUserData, Value, Option<i64>, Option<i64>, Option<u32>, Option<u32>)| {
                {
                    let bytes = data_bytes(&data, "setBuffer")?;
                    let this = borrow_canvas(&ud)?;
                    let mut img = this.img.borrow_mut();
                    let (cw, ch) = img.dimensions();
                    match (x, y, w, h) {
                        (None, None, None, None) => {
                            let expected = cw as u64 * ch as u64 * 4;
                            if bytes.len() as u64 != expected {
                                return Err(LehuaError::msg(format!(
                                    "setBuffer: expected {expected} bytes of RGBA data for {cw}x{ch}, got {}",
                                    bytes.len()
                                ))
                                .into());
                            }
                            *img = RgbaImage::from_raw(cw, ch, bytes)
                                .ok_or_else(|| LehuaError::msg("setBuffer: invalid buffer"))?;
                        }
                        (Some(x), Some(y), Some(w), Some(h)) => {
                            let expected = w as u64 * h as u64 * 4;
                            if bytes.len() as u64 != expected {
                                return Err(LehuaError::msg(format!(
                                    "setBuffer: expected {expected} bytes of RGBA data for a {w}x{h} region, got {}",
                                    bytes.len()
                                ))
                                .into());
                            }
                            for row in 0..h as i64 {
                                let ty = y + row;
                                if ty < 0 || ty >= ch as i64 {
                                    continue;
                                }
                                for col in 0..w as i64 {
                                    let tx = x + col;
                                    if tx < 0 || tx >= cw as i64 {
                                        continue;
                                    }
                                    let i = (row as usize * w as usize + col as usize) * 4;
                                    img.put_pixel(
                                        tx as u32,
                                        ty as u32,
                                        Rgba([bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]]),
                                    );
                                }
                            }
                        }
                        _ => {
                            return Err(LehuaError::msg(
                                "setBuffer: pass no region, or all of x, y, w, h",
                            )
                            .into())
                        }
                    }
                }
                Ok(ud)
            },
        );

        m.add_method("sample", |lua, this, (x, y): (f64, f64)| {
            let img = this.img.borrow();
            color_to_table(lua, sample_bilinear(&img, x, y))
        });

        m.add_method("slice", |lua, this, (tw, th): (u32, u32)| {
            if tw == 0 || th == 0 {
                return Err(LehuaError::msg("slice: tile size must be at least 1x1").into());
            }
            let img = this.img.borrow();
            let (w, h) = img.dimensions();
            let out = lua.create_table()?;
            let mut i = 0;
            let mut y = 0;
            while y < h {
                let cur_h = th.min(h - y);
                let mut x = 0;
                while x < w {
                    let cur_w = tw.min(w - x);
                    let tile = imageops::crop_imm(&*img, x, y, cur_w, cur_h).to_image();
                    i += 1;
                    out.raw_seti(i, from_image(tile))?;
                    x += tw;
                }
                y += th;
            }
            Ok(out)
        });

        m.add_function(
            "halftone",
            |_, (ud, cell, color, background): (AnyUserData, u32, Option<Value>, Option<Value>)| {
                {
                    let this = borrow_canvas(&ud)?;
                    let cell = cell.clamp(2, 256);
                    let fg = opt_color(&color, Rgba([0, 0, 0, 255]))?;
                    let bg = opt_color(&background, Rgba([255, 255, 255, 255]))?;
                    let src = this.img.borrow().clone();
                    let (w, h) = src.dimensions();
                    let mut out = RgbaImage::from_pixel(w, h, bg);
                    let mut y = 0;
                    while y < h {
                        let ch = cell.min(h - y);
                        let mut x = 0;
                        while x < w {
                            let cw = cell.min(w - x);
                            let mut acc = 0.0;
                            for yy in y..y + ch {
                                for xx in x..x + cw {
                                    let p = src.get_pixel(xx, yy);
                                    acc += (255.0 - luminance(p)) * (p.0[3] as f64 / 255.0);
                                }
                            }
                            let darkness = acc / (cw * ch) as f64 / 255.0;
                            let r = darkness.sqrt() * cell as f64 * 0.55;
                            if r >= 0.5 {
                                drawing::draw_filled_circle_mut(
                                    &mut out,
                                    (x as i32 + cw as i32 / 2, y as i32 + ch as i32 / 2),
                                    r.round() as i32,
                                    fg,
                                );
                            }
                            x += cell;
                        }
                        y += cell;
                    }
                    *this.img.borrow_mut() = out;
                }
                Ok(ud)
            },
        );

        m.add_meta_method(MetaMethod::ToString, |_, this, ()| {
            let img = this.img.borrow();
            Ok(format!("Canvas({}x{})", img.width(), img.height()))
        });
    }
}

pub fn build(ctx: &LibCtx) -> mlua::Result<Value> {
    let lua = ctx.lua;
    let t = lua.create_table()?;
    let scope = PathScope::new(ctx);

    t.set(
        "new",
        lua.create_function(|_, (w, h, color): (u32, u32, Option<Value>)| {
            if w == 0 || h == 0 || w > 16384 || h > 16384 {
                return Err(LehuaError::msg("canvas.new: size must be between 1x1 and 16384x16384").into());
            }
            let bg = opt_color(&color, Rgba([0, 0, 0, 0]))?;
            Ok(from_image(RgbaImage::from_pixel(w, h, bg)))
        })?,
    )?;

    t.set(
        "decode",
        lua.create_async_function(|_, data: mlua::LuaString| {
            let bytes = data.as_bytes().to_vec();
            async move { run_blocking(move || decode_bytes(&bytes)).await }
        })?,
    )?;

    t.set(
        "fromPixels",
        lua.create_function(|_, (w, h, data): (u32, u32, Value)| {
            if w == 0 || h == 0 || w > 16384 || h > 16384 {
                return Err(
                    LehuaError::msg("fromPixels: size must be between 1x1 and 16384x16384").into(),
                );
            }
            let bytes = data_bytes(&data, "fromPixels")?;
            let expected = w as u64 * h as u64 * 4;
            if bytes.len() as u64 != expected {
                return Err(LehuaError::msg(format!(
                    "fromPixels: expected {expected} bytes of RGBA data for {w}x{h}, got {}",
                    bytes.len()
                ))
                .into());
            }
            let img = RgbaImage::from_raw(w, h, bytes)
                .ok_or_else(|| LehuaError::msg("fromPixels: invalid buffer"))?;
            Ok(from_image(img))
        })?,
    )?;

    t.set(
        "checker",
        lua.create_function(
            |_, (w, h, size, color1, color2): (u32, u32, Option<u32>, Option<Value>, Option<Value>)| {
                if w == 0 || h == 0 || w > 16384 || h > 16384 {
                    return Err(LehuaError::msg(
                        "canvas.checker: size must be between 1x1 and 16384x16384",
                    )
                    .into());
                }
                let size = size.unwrap_or(8).max(1);
                let a = opt_color(&color1, Rgba([204, 204, 204, 255]))?;
                let b = opt_color(&color2, Rgba([255, 255, 255, 255]))?;
                let mut img = RgbaImage::new(w, h);
                for (x, y, p) in img.enumerate_pixels_mut() {
                    *p = if ((x / size) + (y / size)) % 2 == 0 { a } else { b };
                }
                Ok(from_image(img))
            },
        )?,
    )?;

    t.set(
        "montage",
        lua.create_function(|_, (items, opts): (Vec<AnyUserData>, Option<Table>)| {
            if items.is_empty() {
                return Err(LehuaError::msg("canvas.montage: needs at least one canvas").into());
            }
            let mut cols = 0usize;
            let mut gap = 0u32;
            let mut bg = Rgba([0, 0, 0, 0]);
            if let Some(o) = &opts {
                if let Some(c) = o.get::<Option<u32>>("cols")? {
                    cols = c as usize;
                }
                if let Some(g) = o.get::<Option<u32>>("gap")? {
                    gap = g.min(16384);
                }
                let bgv: Value = o.get("background")?;
                if !bgv.is_nil() {
                    bg = parse_color(&bgv)?;
                }
            }
            let sources: Vec<RgbaImage> = items
                .iter()
                .map(|ud| Ok(borrow_canvas(ud)?.img.borrow().clone()))
                .collect::<mlua::Result<_>>()?;
            let n = sources.len();
            if cols == 0 {
                cols = (n as f64).sqrt().ceil() as usize;
            }
            let cols = cols.max(1).min(n);
            let rows = n.div_ceil(cols);
            let cell_w = sources.iter().map(|i| i.width()).max().unwrap_or(1).max(1);
            let cell_h = sources.iter().map(|i| i.height()).max().unwrap_or(1).max(1);
            let total_w = cols as u64 * cell_w as u64 + (cols as u64 - 1) * gap as u64;
            let total_h = rows as u64 * cell_h as u64 + (rows as u64 - 1) * gap as u64;
            if total_w > 16384 || total_h > 16384 {
                return Err(LehuaError::msg(format!(
                    "canvas.montage: result would be {total_w}x{total_h}, the limit is 16384x16384"
                ))
                .into());
            }
            let mut out = RgbaImage::from_pixel(total_w as u32, total_h as u32, bg);
            for (i, src) in sources.iter().enumerate() {
                let col = (i % cols) as u32;
                let row = (i / cols) as u32;
                let ox = col * (cell_w + gap) + (cell_w - src.width()) / 2;
                let oy = row * (cell_h + gap) + (cell_h - src.height()) / 2;
                for (x, y, sp) in src.enumerate_pixels() {
                    let dp = *out.get_pixel(ox + x, oy + y);
                    out.put_pixel(ox + x, oy + y, composite_pixel(dp, *sp, Blend::Normal, 1.0));
                }
            }
            Ok(from_image(out))
        })?,
    )?;

    {
        let scope = Rc::clone(&scope);
        t.set(
            "font",
            lua.create_async_function(move |_, path: String| {
                let scope = scope.clone();
                async move {
                    let full = scope.resolve(&path)?;
                    run_blocking(move || {
                        let bytes = std::fs::read(&full).map_err(|e| {
                            LehuaError::msg(format!(
                                "could not read font '{}': {e}",
                                full.display()
                            ))
                        })?;
                        let font = FontVec::try_from_vec(bytes).map_err(|_| {
                            LehuaError::msg(format!(
                                "'{}' is not a valid font",
                                full.display()
                            ))
                        })?;
                        Ok(FontObj { font })
                    })
                    .await
                }
            })?,
        )?;
    }

    t.set(
        "rgb",
        lua.create_function(|lua, (r, g, b): (f64, f64, f64)| {
            color_to_table(lua, Rgba([clamp_channel(r), clamp_channel(g), clamp_channel(b), 255]))
        })?,
    )?;

    t.set(
        "rgba",
        lua.create_function(|lua, (r, g, b, a): (f64, f64, f64, f64)| {
            color_to_table(
                lua,
                Rgba([clamp_channel(r), clamp_channel(g), clamp_channel(b), clamp_channel(a)]),
            )
        })?,
    )?;

    t.set(
        "hsv",
        lua.create_function(|lua, (h, s, v, a): (f64, f64, f64, Option<f64>)| {
            let rgb = hsv_to_rgb(h, s, v);
            color_to_table(
                lua,
                Rgba([rgb[0], rgb[1], rgb[2], clamp_channel(a.unwrap_or(255.0))]),
            )
        })?,
    )?;

    t.set(
        "hex",
        lua.create_function(|lua, s: String| {
            let c = parse_hex(&s)
                .ok_or_else(|| LehuaError::msg(format!("invalid hex color '{s}'")))?;
            color_to_table(lua, Rgba(c))
        })?,
    )?;

    t.set(
        "noise",
        lua.create_function(|_, opts: Option<Table>| {
            let mut seed_bytes = [0u8; 8];
            let _ = getrandom::fill(&mut seed_bytes);
            let core = Rc::new(NoiseCore {
                perm: RefCell::new(build_perm(u64::from_le_bytes(seed_bytes))),
                params: RefCell::new(NoiseParams {
                    seed: u64::from_le_bytes(seed_bytes),
                    scale: 64.0,
                    octaves: 4,
                    persistence: 0.5,
                    lacunarity: 2.0,
                    kind: NoiseKind::Perlin,
                }),
                warp: RefCell::new(None),
            });
            if let Some(o) = &opts {
                apply_noise_params(&core, o)?;
            }
            Ok(NoiseObj { core })
        })?,
    )?;

    t.set(
        "formats",
        lua.create_function(|lua, ()| {
            let t = lua.create_table()?;
            for (i, f) in ["png", "jpg", "gif", "bmp", "ico", "tiff", "webp", "qoi", "tga"]
                .iter()
                .enumerate()
            {
                t.raw_seti(i + 1, *f)?;
            }
            Ok(t)
        })?,
    )?;

    Ok(Value::Table(t))
}
