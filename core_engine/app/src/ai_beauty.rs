// ai_beauty.rs  -  Neural Beauty (path-tracer / G-buffer guided AI polish)
//
// Flow:
//   1. UI requests capture -> flag set
//   2. viewport_render copies LDR beauty + gbuf_normal (+ curvature) to staging
//   3. On map complete -> decode f16 AOVs -> PNG base64 -> worker POST /neural_beauty
//   4. Poll returns polished RGBA for overlay / export

use eframe::egui;
use eframe::wgpu;
use image::ImageEncoder;
use super::{AzraApp, DirtyFlags};
use crate::gpu_ctx::PendingAiBeautyCapture;
use crate::AiState;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::{Arc, atomic::AtomicBool};

// ---------------------------------------------------------------------------
// Result type
// ---------------------------------------------------------------------------
pub(crate) struct AiBeautyResult {
    pub(crate) pixels: Vec<u32>, // packed RGBA8
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) elapsed: f32,
    pub(crate) mode: String,
    pub(crate) png_bytes: Vec<u8>,
}

// ---------------------------------------------------------------------------
// f16 helpers (IEEE half -> f32)
// ---------------------------------------------------------------------------
#[inline]
pub(crate) fn f16_bits_to_f32(h: u16) -> f32 {
    let s = ((h >> 15) & 1) as u32;
    let e = ((h >> 10) & 0x1F) as u32;
    let f = (h & 0x3FF) as u32;
    if e == 0 {
        if f == 0 {
            return if s == 0 { 0.0 } else { -0.0 };
        }
        // subnormal
        let mut mant = f as f32 / 1024.0;
        let mut exp = -14i32;
        while mant < 1.0 {
            mant *= 2.0;
            exp -= 1;
        }
        mant -= 1.0;
        let bits = (s << 31) | (((exp + 127) as u32) << 23) | ((mant * (1 << 23) as f32) as u32 & 0x7FFFFF);
        return f32::from_bits(bits);
    }
    if e == 31 {
        return if f == 0 {
            if s == 0 { f32::INFINITY } else { f32::NEG_INFINITY }
        } else {
            f32::NAN
        };
    }
    let bits = (s << 31) | ((e + 127 - 15) << 23) | (f << 13);
    f32::from_bits(bits)
}

fn read_f16_pixel(row: &[u8], x: usize) -> [f32; 4] {
    let o = x * 8;
    if o + 8 > row.len() {
        return [0.0; 4];
    }
    let r = u16::from_le_bytes([row[o], row[o + 1]]);
    let g = u16::from_le_bytes([row[o + 2], row[o + 3]]);
    let b = u16::from_le_bytes([row[o + 4], row[o + 5]]);
    let a = u16::from_le_bytes([row[o + 6], row[o + 7]]);
    [
        f16_bits_to_f32(r),
        f16_bits_to_f32(g),
        f16_bits_to_f32(b),
        f16_bits_to_f32(a),
    ]
}

fn tonemap_channel(v: f32) -> u8 {
    // Simple clamp  -  LDR path is already tonemapped into ~0..1
    let x = if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 };
    (x * 255.0 + 0.5) as u8
}

fn normal_to_u8(n: f32) -> u8 {
    // Assume world normals roughly in [-1,1]
    let x = if n.is_finite() { (n * 0.5 + 0.5).clamp(0.0, 1.0) } else { 0.5 };
    (x * 255.0 + 0.5) as u8
}

// ---------------------------------------------------------------------------
// AOV decode -> PNG bytes
// ---------------------------------------------------------------------------
pub(crate) struct BeautyAovs {
    pub beauty_png: Vec<u8>,
    pub depth_png: Vec<u8>,
    pub normal_png: Vec<u8>,
    pub edges_png: Vec<u8>,
    pub curvature_png: Vec<u8>,
    pub beauty_rgba: Vec<u8>, // raw RGBA for local before preview
    pub width: u32,
    pub height: u32,
}

/// Crop RGBA8 buffer to a sub-rect (x,y,w,h) in pixel space.
fn crop_rgba8(src: &[u8], full_w: u32, full_h: u32, x: u32, y: u32, w: u32, h: u32) -> Vec<u8> {
    let fw = full_w as usize;
    let fh = full_h as usize;
    let x = x.min(full_w) as usize;
    let y = y.min(full_h) as usize;
    let w = w.min(full_w.saturating_sub(x as u32)) as usize;
    let h = h.min(full_h.saturating_sub(y as u32)) as usize;
    if w == 0 || h == 0 || src.len() < fw * fh * 4 {
        return src.to_vec();
    }
    let mut out = vec![0u8; w * h * 4];
    for row in 0..h {
        let src_off = ((y + row) * fw + x) * 4;
        let dst_off = row * w * 4;
        let n = w * 4;
        if src_off + n <= src.len() && dst_off + n <= out.len() {
            out[dst_off..dst_off + n].copy_from_slice(&src[src_off..src_off + n]);
        }
    }
    out
}

fn reencode_png_rgba(rgba: &[u8], w: u32, h: u32) -> Result<Vec<u8>, String> {
    let img = image::RgbaImage::from_raw(w, h, rgba.to_vec())
        .ok_or_else(|| "crop encode: bad dimensions".to_string())?;
    let mut buf = Vec::new();
    let enc = image::codecs::png::PngEncoder::new(&mut buf);
    use image::ImageEncoder;
    enc.write_image(img.as_raw(), w, h, image::ExtendedColorType::Rgba8)
        .map_err(|e| format!("PNG encode: {e}"))?;
    Ok(buf)
}

/// Crop all AOVs to the AI render region so resolution budget goes to the subject.
pub(crate) fn crop_beauty_aovs(aovs: BeautyAovs, x: u32, y: u32, rw: u32, rh: u32) -> Result<BeautyAovs, String> {
    if x == 0 && y == 0 && rw == aovs.width && rh == aovs.height {
        return Ok(aovs);
    }
    if rw < 32 || rh < 32 {
        return Ok(aovs);
    }
    let beauty_rgba = crop_rgba8(&aovs.beauty_rgba, aovs.width, aovs.height, x, y, rw, rh);
    // PNGs were encoded full-frame — re-decode via crop of rgba only for beauty; re-encode others from PNG decode would be heavy.
    // Simpler: re-crop by decoding png to rgba is overkill. We only have beauty_rgba raw; re-encode all from cropped raw if we had them.
    // Rebuild PNGs by cropping decoded full-frame PNGs:
    let crop_png = |png: &[u8]| -> Result<Vec<u8>, String> {
        if png.is_empty() {
            return Ok(Vec::new());
        }
        let img = image::load_from_memory(png).map_err(|e| format!("png load: {e}"))?;
        let rgba = img.to_rgba8();
        let cropped = crop_rgba8(rgba.as_raw(), aovs.width, aovs.height, x, y, rw, rh);
        reencode_png_rgba(&cropped, rw, rh)
    };
    Ok(BeautyAovs {
        beauty_png: reencode_png_rgba(&beauty_rgba, rw, rh)?,
        depth_png: crop_png(&aovs.depth_png)?,
        normal_png: crop_png(&aovs.normal_png)?,
        edges_png: crop_png(&aovs.edges_png)?,
        curvature_png: crop_png(&aovs.curvature_png)?,
        beauty_rgba,
        width: rw,
        height: rh,
    })
}

/// Decode staged f16 beauty + normal/depth (+ optional curvature) into PNG AOVs.
pub(crate) fn decode_aovs_from_staging(
    width: u32,
    height: u32,
    bpr: u32,
    beauty_raw: &[u8],
    normal_raw: &[u8],
    curvature_raw: Option<&[u8]>,
) -> Result<BeautyAovs, String> {
    let w = width as usize;
    let h = height as usize;
    let mut beauty_rgba = vec![0u8; w * h * 4];
    let mut depth_rgba = vec![0u8; w * h * 4];
    let mut normal_rgba = vec![0u8; w * h * 4];
    let mut edges_rgba = vec![0u8; w * h * 4];
    let mut curv_rgba = vec![0u8; w * h * 4];

    // Pass 1: beauty + normal + collect depth values for normalization
    let mut depths = Vec::with_capacity(w * h);
    for y in 0..h {
        let row_off = y * bpr as usize;
        let b_row = &beauty_raw[row_off.min(beauty_raw.len())..];
        let n_row = &normal_raw[row_off.min(normal_raw.len())..];
        for x in 0..w {
            let bp = read_f16_pixel(b_row, x);
            let i = (y * w + x) * 4;
            beauty_rgba[i] = tonemap_channel(bp[0]);
            beauty_rgba[i + 1] = tonemap_channel(bp[1]);
            beauty_rgba[i + 2] = tonemap_channel(bp[2]);
            beauty_rgba[i + 3] = 255;

            let np = read_f16_pixel(n_row, x);
            normal_rgba[i] = normal_to_u8(np[0]);
            normal_rgba[i + 1] = normal_to_u8(np[1]);
            normal_rgba[i + 2] = normal_to_u8(np[2]);
            normal_rgba[i + 3] = 255;

            let d = np[3];
            if d.is_finite() && d > 0.0 && d < 1.0e6 {
                depths.push(d);
            } else {
                depths.push(0.0);
            }
        }
    }

    // Depth normalize (ignore zeros = miss)
    let mut finite: Vec<f32> = depths.iter().copied().filter(|&d| d > 1e-4).collect();
    finite.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let (d_min, d_max) = if finite.len() > 8 {
        let lo = finite[finite.len() / 20]; // 5th percentile
        let hi = finite[finite.len().saturating_sub(1 + finite.len() / 20)];
        (lo, hi.max(lo + 1e-3))
    } else {
        (0.0, 1.0)
    };

    for y in 0..h {
        for x in 0..w {
            let idx = y * w + x;
            let i = idx * 4;
            let d = depths[idx];
            let t = if d > 1e-4 {
                ((d - d_min) / (d_max - d_min)).clamp(0.0, 1.0)
            } else {
                1.0 // far / miss -> white (background)
            };
            let v = (t * 255.0) as u8;
            depth_rgba[i] = v;
            depth_rgba[i + 1] = v;
            depth_rgba[i + 2] = v;
            depth_rgba[i + 3] = 255;
        }
    }

    // Curvature (optional)
    if let Some(craw) = curvature_raw {
        for y in 0..h {
            let row_off = y * bpr as usize;
            let c_row = &craw[row_off.min(craw.len())..];
            for x in 0..w {
                let cp = read_f16_pixel(c_row, x);
                let i = (y * w + x) * 4;
                // Pack |curvature| magnitude into grayscale
                let m = (cp[0].abs() + cp[1].abs() + cp[2].abs()) * 0.5;
                let v = tonemap_channel(m);
                curv_rgba[i] = v;
                curv_rgba[i + 1] = v;
                curv_rgba[i + 2] = v;
                curv_rgba[i + 3] = 255;
            }
        }
    }

    // Silhouette / edges from depth + normal discontinuities
    for y in 0..h {
        for x in 0..w {
            let i = (y * w + x) * 4;
            let d0 = depths[y * w + x];
            let n0 = [
                normal_rgba[i] as f32,
                normal_rgba[i + 1] as f32,
                normal_rgba[i + 2] as f32,
            ];
            let mut edge = 0.0f32;
            for (dx, dy) in [(1i32, 0i32), (0, 1), (-1, 0), (0, -1)] {
                let nx = x as i32 + dx;
                let ny = y as i32 + dy;
                if nx < 0 || ny < 0 || nx >= w as i32 || ny >= h as i32 {
                    edge = edge.max(1.0);
                    continue;
                }
                let j = (ny as usize * w + nx as usize) * 4;
                let d1 = depths[ny as usize * w + nx as usize];
                if d0 > 1e-4 && d1 > 1e-4 {
                    let rel = ((d0 - d1).abs() / d0.max(1e-3)).min(1.0);
                    edge = edge.max(rel * 4.0);
                } else if (d0 > 1e-4) != (d1 > 1e-4) {
                    edge = edge.max(1.0);
                }
                let n1 = [
                    normal_rgba[j] as f32,
                    normal_rgba[j + 1] as f32,
                    normal_rgba[j + 2] as f32,
                ];
                let nd = ((n0[0] - n1[0]).abs() + (n0[1] - n1[1]).abs() + (n0[2] - n1[2]).abs())
                    / (3.0 * 255.0);
                edge = edge.max(nd * 3.0);
            }
            let v = (edge.clamp(0.0, 1.0) * 255.0) as u8;
            edges_rgba[i] = v;
            edges_rgba[i + 1] = v;
            edges_rgba[i + 2] = v;
            edges_rgba[i + 3] = 255;
        }
    }

    let encode = |rgba: &[u8]| -> Result<Vec<u8>, String> {
        let img = image::RgbaImage::from_raw(width, height, rgba.to_vec())
            .ok_or_else(|| "AOV encode: bad dimensions".to_string())?;
        let mut buf = Vec::new();
        let enc = image::codecs::png::PngEncoder::new(&mut buf);
        use image::ImageEncoder;
        enc.write_image(img.as_raw(), width, height, image::ExtendedColorType::Rgba8)
            .map_err(|e| format!("PNG encode: {e}"))?;
        Ok(buf)
    };

    Ok(BeautyAovs {
        beauty_png: encode(&beauty_rgba)?,
        depth_png: encode(&depth_rgba)?,
        normal_png: encode(&normal_rgba)?,
        edges_png: encode(&edges_rgba)?,
        curvature_png: encode(&curv_rgba)?,
        beauty_rgba,
        width,
        height,
    })
}

// ---------------------------------------------------------------------------
// HTTP client
// ---------------------------------------------------------------------------
fn base64_encode(data: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity((data.len() + 2) / 3 * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHABET[((triple >> 18) & 0x3F) as usize] as char);
        out.push(ALPHABET[((triple >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            out.push(ALPHABET[((triple >> 6) & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(ALPHABET[(triple & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

fn extract_json_string(body: &str, key: &str) -> Option<String> {
    let pattern = format!("\"{}\":\"", key);
    let start = body.find(&pattern)? + pattern.len();
    let rest = &body[start..];
    let mut out = String::new();
    let mut chars = rest.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('"') => out.push('"'),
                Some('\\') => out.push('\\'),
                Some('n') => out.push('\n'),
                Some('r') => out.push('\r'),
                Some('t') => out.push('\t'),
                Some(o) => out.push(o),
                None => break,
            }
        } else if c == '"' {
            break;
        } else {
            out.push(c);
        }
    }
    Some(out)
}

fn extract_json_f32(body: &str, key: &str) -> Option<f32> {
    let pattern = format!("\"{}\":", key);
    let start = body.find(&pattern)? + pattern.len();
    let rest = body[start..].trim_start();
    let end = rest
        .find(|c: char| !c.is_ascii_digit() && c != '.' && c != '-' && c != 'e' && c != 'E' && c != '+')
        .unwrap_or(rest.len());
    rest[..end].parse().ok()
}

fn extract_json_u32(body: &str, key: &str) -> Option<u32> {
    extract_json_f32(body, key).map(|v| v as u32)
}

fn base64_decode(input: &str) -> Result<Vec<u8>, String> {
    fn val(c: u8) -> Option<u8> {
        match c {
            b'A'..=b'Z' => Some(c - b'A'),
            b'a'..=b'z' => Some(c - b'a' + 26),
            b'0'..=b'9' => Some(c - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            b'=' => Some(0),
            _ => None,
        }
    }
    let bytes: Vec<u8> = input.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    if bytes.len() % 4 != 0 {
        return Err("bad base64 length".into());
    }
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    for chunk in bytes.chunks(4) {
        let a = val(chunk[0]).ok_or("bad b64")?;
        let b = val(chunk[1]).ok_or("bad b64")?;
        let c = val(chunk[2]).ok_or("bad b64")?;
        let d = val(chunk[3]).ok_or("bad b64")?;
        let n = ((a as u32) << 18) | ((b as u32) << 12) | ((c as u32) << 6) | (d as u32);
        out.push(((n >> 16) & 0xFF) as u8);
        if chunk[2] != b'=' {
            out.push(((n >> 8) & 0xFF) as u8);
        }
        if chunk[3] != b'=' {
            out.push((n & 0xFF) as u8);
        }
    }
    Ok(out)
}

pub(crate) struct NeuralBeautyRequest {
    pub host: String,
    pub port: u16,
    pub prompt: String,
    pub strength: f32,
    pub structure: f32,
    pub seed: u32,
    pub steps: u32,
    pub guidance: f32,
    pub max_side: u32,
    pub quality_preset: String,
    pub use_depth: bool,
    pub use_normal: bool,
    pub use_edges: bool,
    pub use_curvature: bool,
    pub beauty_png: Vec<u8>,
    pub depth_png: Vec<u8>,
    pub normal_png: Vec<u8>,
    pub edges_png: Vec<u8>,
    pub curvature_png: Vec<u8>,
}

pub(crate) fn request_neural_beauty(req: &NeuralBeautyRequest) -> Result<AiBeautyResult, String> {
    let addr = format!("{}:{}", req.host, req.port);
    let sock: std::net::SocketAddr = addr
        .parse()
        .map_err(|e| format!("Bad address: {e}"))?;
    let mut stream = TcpStream::connect_timeout(&sock, std::time::Duration::from_secs(5))
        .map_err(|e| format!(
            "Cannot connect to {addr} — Azra AI pipeline not up ({e})\n\
             Tip: restart Azra, or run: python ai_backend/server/texture_server.py --model flux-kontext --port 7860"
        ))?;
    // Flux.1 Kontext beauty can run several minutes at 1024+ with cpu_offload.
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(900)))
        .ok();
    stream
        .set_write_timeout(Some(std::time::Duration::from_secs(180)))
        .ok();

    let body = format!(
        r#"{{"prompt":"{}","strength":{},"structure":{},"seed":{},"steps":{},"guidance":{},"max_side":{},"quality_preset":"{}","use_depth":{},"use_normal":{},"use_edges":{},"use_curvature":{},"beauty_b64":"{}","depth_b64":"{}","normal_b64":"{}","edges_b64":"{}","curvature_b64":"{}"}}"#,
        AzraApp::json_escape(&req.prompt),
        req.strength,
        req.structure,
        req.seed,
        req.steps,
        req.guidance,
        req.max_side,
        AzraApp::json_escape(&req.quality_preset),
        if req.use_depth { "true" } else { "false" },
        if req.use_normal { "true" } else { "false" },
        if req.use_edges { "true" } else { "false" },
        if req.use_curvature { "true" } else { "false" },
        base64_encode(&req.beauty_png),
        base64_encode(&req.depth_png),
        base64_encode(&req.normal_png),
        base64_encode(&req.edges_png),
        base64_encode(&req.curvature_png),
    );

    let request = format!(
        "POST /neural_beauty HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        addr,
        body.len(),
        body
    );
    stream
        .write_all(request.as_bytes())
        .map_err(|e| format!(
            "Write failed ({e}). Server may have closed the connection — restart texture_server."
        ))?;

    let mut response_bytes = Vec::new();
    stream
        .read_to_end(&mut response_bytes)
        .map_err(|e| {
            let msg = e.to_string();
            if msg.contains("10054") || msg.contains("forcibly closed") || msg.contains("reset") {
                format!(
                    "Server closed the connection (os error 10054).\n\
                     The texture server is likely outdated or crashed.\n\
                     Fix: close Azra AI processes on port 7860 and restart Azra,\n\
                     or run: python F:\\PROJECT\\FourthLAyerAi\\ai_backend\\server\\texture_server.py --port 7860"
                )
            } else {
                format!("Read: {msg}")
            }
        })?;
    let response = String::from_utf8_lossy(&response_bytes).to_string();
    let body_start = response
        .find("\r\n\r\n")
        .ok_or("No HTTP headers in neural_beauty response")?;
    let http_body = &response[body_start + 4..];
    let status_line = response.lines().next().unwrap_or("");

    if status_line.contains("404") {
        return Err(
            "Server missing /neural_beauty (404).\n\
             An OLD texture_server is still running without this endpoint.\n\
             Kill python on port 7860 and restart Azra (or re-run texture_server.py)."
                .into(),
        );
    }

    if status_line.contains(" 5") || http_body.contains("\"error\"") {
        let err = extract_json_string(http_body, "error")
            .unwrap_or_else(|| http_body.chars().take(200).collect());
        return Err(err);
    }

    let png_b64 = extract_json_string(http_body, "result_png_b64")
        .or_else(|| extract_json_string(http_body, "result_rgba_b64"))
        .ok_or_else(|| {
            format!(
                "No result image in response. Status: {}. Body: {}",
                status_line,
                http_body.chars().take(180).collect::<String>()
            )
        })?;
    let png_bytes = base64_decode(&png_b64)?;

    // Prefer decoded PNG; fall back to raw RGBA if server sent that
    let (width, height, pixels) = if png_bytes.len() > 8 && &png_bytes[0..4] == b"\x89PNG" {
        let img = image::load_from_memory(&png_bytes).map_err(|e| format!("decode result: {e}"))?;
        let rgba = img.to_rgba8();
        let (w, h) = rgba.dimensions();
        let pixels: Vec<u32> = rgba
            .chunks_exact(4)
            .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();
        (w, h, pixels)
    } else {
        let w = extract_json_u32(http_body, "width").unwrap_or(0);
        let h = extract_json_u32(http_body, "height").unwrap_or(0);
        if w == 0 || h == 0 || png_bytes.len() < (w * h * 4) as usize {
            return Err("Invalid raw RGBA result".into());
        }
        let pixels: Vec<u32> = png_bytes[..(w * h * 4) as usize]
            .chunks_exact(4)
            .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();
        (w, h, pixels)
    };

    let elapsed = extract_json_f32(http_body, "elapsed").unwrap_or(0.0);
    let mode = extract_json_string(http_body, "mode").unwrap_or_else(|| "neural_beauty".into());

    let out_png = if png_bytes.len() > 8 && &png_bytes[0..4] == b"\x89PNG" {
        png_bytes
    } else {
        // re-encode raw RGBA for save
        let mut buf = Vec::new();
        let raw: Vec<u8> = pixels.iter().flat_map(|p| p.to_le_bytes()).collect();
        if let Some(img) = image::RgbaImage::from_raw(width, height, raw) {
            let _ = image::codecs::png::PngEncoder::new(&mut buf).write_image(
                img.as_raw(),
                width,
                height,
                image::ExtendedColorType::Rgba8,
            );
        }
        buf
    };

    Ok(AiBeautyResult {
        pixels,
        width,
        height,
        elapsed,
        mode,
        png_bytes: out_png,
    })
}

// ---------------------------------------------------------------------------
// GPU capture kickoff
// ---------------------------------------------------------------------------
impl AzraApp {
    /// Call when starting an AI job: free GPU for Flux (pause PT, toast once).
    pub(crate) fn begin_ai_gpu_exclusive(&mut self, job: &str) {
        if !self.ai.ai_gpu_yield_notified {
            self.ai.ai_gpu_yield_notified = true;
            let was_pt = matches!(self.render.shading_mode, crate::ShadingMode::PathTrace);
            if was_pt {
                self.toast(&format!(
                    "AI owns GPU — Path Trace paused for {job} (no dual load on 4090)"
                ));
            } else {
                self.toast(&format!("AI owns GPU — {job}"));
            }
            eprintln!("[Azra] AI GPU exclusive ON ({job}) — PathTrace/CUDA PT yielded");
        }
        // Drop progressive PT accumulation so we don't thrash history when resuming.
        self.render.pt_accumulated_frames = 0;
        self.mark_dirty(DirtyFlags::RENDER);
    }

    /// Clear exclusive toast latch when no AI job remains.
    pub(crate) fn end_ai_gpu_exclusive_if_idle(&mut self) {
        if !self.ai.ai_gpu_exclusive() && self.ai.ai_gpu_yield_notified {
            self.ai.ai_gpu_yield_notified = false;
            eprintln!("[Azra] AI GPU exclusive OFF — PathTrace may resume");
            self.toast("AI done — Path Trace can resume");
            self.mark_dirty(DirtyFlags::RENDER);
        }
    }

    /// Stop Neural Beauty job (UI cancel).
    pub(crate) fn stop_ai_beauty_job(&mut self, reason: &str) {
        if let Some(ref abort) = self.ai.beauty_abort {
            abort.store(true, std::sync::atomic::Ordering::SeqCst);
        }
        self.ai.beauty_generation = self.ai.beauty_generation.wrapping_add(1);
        self.ai.beauty_rx = None;
        self.ai.beauty_abort = None;
        self.ai.beauty_busy = false;
        self.ai.beauty_capture_requested = false;
        self.ai.pending_beauty_capture = None;
        self.ai.beauty_status = Some(format!("Stopped — {reason}"));
        self.end_ai_gpu_exclusive_if_idle();
    }

    /// Safe clear beauty result (stops job first).
    pub(crate) fn clear_ai_beauty(&mut self) {
        if self.ai.beauty_busy
            || self.ai.beauty_rx.is_some()
            || self.ai.beauty_capture_requested
            || self.ai.pending_beauty_capture.is_some()
        {
            self.stop_ai_beauty_job("clear");
        }
        self.ai.beauty_result_handle = None;
        self.ai.beauty_source_handle = None;
        self.ai.beauty_result_png.clear();
        self.ai.beauty_source_rgba.clear();
        self.ai.beauty_overlay_enabled = false;
        self.ai.beauty_result_w = 0;
        self.ai.beauty_result_h = 0;
        self.ai.beauty_status = Some("Cleared".into());
    }

    /// Request a one-frame multi-AOV capture for Neural Beauty.
    pub(crate) fn request_ai_beauty_capture(&mut self) {
        if self.ai.beauty_busy {
            self.toast("AI Beauty already running...");
            return;
        }
        self.begin_ai_gpu_exclusive("Neural Beauty");
        // Ensure texture_server is up BEFORE capture (GPU capture is wasted if HTTP fails).
        let port = self.ai.ai_tex_port;
        self.ai.beauty_status = Some(format!("Checking AI server on :{port}..."));
        let (ok, msg) = crate::gpu_init::ensure_texture_server(port);
        if !ok {
            self.ai.beauty_status = Some(format!("X {msg}"));
            self.toast("AI server offline — starting failed. See status in AI Beauty window.");
            eprintln!("[AI Beauty] ensure_texture_server failed: {msg}");
            return;
        }
        self.ai.beauty_capture_requested = true;
        self.ai.beauty_status = Some(format!("{msg} — capturing AOVs..."));
        self.toast("AI Beauty: server OK, capturing AOVs...");
    }

    /// Called from viewport_render when beauty_capture_requested and no pending capture.
    pub(crate) fn begin_ai_beauty_gpu_capture(
        ai: &mut AiState,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        beauty: &wgpu::Texture,
        normal: &wgpu::Texture,
        curvature: Option<&wgpu::Texture>,
        width: u32,
        height: u32,
    ) {
        // f16 RGBA -> 8 bytes/pixel
        let bpr = (8 * width + 255) & !255;
        let buf_size = (bpr * height) as u64;

        let make_staging = |label: &str| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: buf_size,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            })
        };

        let beauty_stg = make_staging("ai_beauty_beauty");
        let normal_stg = make_staging("ai_beauty_normal");
        let curv_stg = curvature.map(|_| make_staging("ai_beauty_curvature"));

        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("ai_beauty_aov_enc"),
        });

        let copy = |enc: &mut wgpu::CommandEncoder, src: &wgpu::Texture, dst: &wgpu::Buffer| {
            enc.copy_texture_to_buffer(
                src.as_image_copy(),
                wgpu::ImageCopyBuffer {
                    buffer: dst,
                    layout: wgpu::ImageDataLayout {
                        offset: 0,
                        bytes_per_row: Some(bpr),
                        rows_per_image: Some(height),
                    },
                },
                wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
            );
        };

        copy(&mut enc, beauty, &beauty_stg);
        copy(&mut enc, normal, &normal_stg);
        if let (Some(c), Some(ref stg)) = (curvature, &curv_stg) {
            copy(&mut enc, c, stg);
        }
        queue.submit(Some(enc.finish()));

        let map_one = |buf: &wgpu::Buffer| {
            let (tx, rx) = std::sync::mpsc::channel();
            buf.slice(..).map_async(wgpu::MapMode::Read, move |r| {
                let _ = tx.send(r);
            });
            rx
        };

        let beauty_rx = map_one(&beauty_stg);
        let normal_rx = map_one(&normal_stg);
        let curv_rx = curv_stg.as_ref().map(map_one);

        ai.pending_beauty_capture = Some(PendingAiBeautyCapture {
            beauty_staging: beauty_stg,
            normal_staging: normal_stg,
            curvature_staging: curv_stg,
            beauty_rx,
            normal_rx,
            curvature_rx: curv_rx,
            width,
            height,
            bpr,
            beauty_ready: None,
            normal_ready: None,
            curvature_ready: None,
        });
        ai.beauty_capture_requested = false;
        ai.beauty_status = Some("Reading AOVs from GPU...".into());
    }

    /// Poll in-flight AOV capture; when complete, spawn HTTP worker.
    pub(crate) fn poll_ai_beauty_capture(&mut self, ctx: &egui::Context) {
        let Some(mut pending) = self.ai.pending_beauty_capture.take() else {
            return;
        };

        // Drive map callbacks
        if let Some(gpu) = self.gpu.as_ref() {
            gpu.device.poll(wgpu::PollType::Poll).expect("GPU device polling failed");
        }

        if pending.beauty_ready.is_none() {
            match pending.beauty_rx.try_recv() {
                Ok(Ok(())) => {
                    let data = pending.beauty_staging.slice(..).get_mapped_range().to_vec();
                    pending.beauty_staging.unmap();
                    pending.beauty_ready = Some(data);
                }
                Ok(Err(_)) => {
                    self.ai.beauty_status = Some(" Beauty AOV map failed".into());
                    return;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    self.ai.pending_beauty_capture = Some(pending);
                    ctx.request_repaint();
                    return;
                }
                Err(_) => {
                    self.ai.beauty_status = Some(" Beauty AOV channel closed".into());
                    return;
                }
            }
        }

        if pending.normal_ready.is_none() {
            match pending.normal_rx.try_recv() {
                Ok(Ok(())) => {
                    let data = pending.normal_staging.slice(..).get_mapped_range().to_vec();
                    pending.normal_staging.unmap();
                    pending.normal_ready = Some(data);
                }
                Ok(Err(_)) => {
                    self.ai.beauty_status = Some(" Normal/depth AOV map failed".into());
                    return;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    self.ai.pending_beauty_capture = Some(pending);
                    ctx.request_repaint();
                    return;
                }
                Err(_) => {
                    self.ai.beauty_status = Some(" Normal AOV channel closed".into());
                    return;
                }
            }
        }

        if pending.curvature_staging.is_some() && pending.curvature_ready.is_none() {
            if let Some(ref rx) = pending.curvature_rx {
                match rx.try_recv() {
                    Ok(Ok(())) => {
                        if let Some(ref stg) = pending.curvature_staging {
                            let data = stg.slice(..).get_mapped_range().to_vec();
                            stg.unmap();
                            pending.curvature_ready = Some(data);
                        }
                    }
                    Ok(Err(_)) => {
                        // Non-fatal  -  continue without curvature
                        pending.curvature_ready = Some(Vec::new());
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => {
                        self.ai.pending_beauty_capture = Some(pending);
                        ctx.request_repaint();
                        return;
                    }
                    Err(_) => {
                        pending.curvature_ready = Some(Vec::new());
                    }
                }
            }
        }

        // All ready
        let beauty_raw = pending.beauty_ready.take().unwrap_or_default();
        let normal_raw = pending.normal_ready.take().unwrap_or_default();
        let curv_raw = pending.curvature_ready.take().filter(|v| !v.is_empty());
        let (w, h, bpr) = (pending.width, pending.height, pending.bpr);

        match decode_aovs_from_staging(
            w,
            h,
            bpr,
            &beauty_raw,
            &normal_raw,
            curv_raw.as_deref(),
        ) {
            Ok(aovs) => {
                // Crop to AI render region so max_side budget is spent on the subject.
                let (rx, ry, rw, rh) = self.ai.beauty_region_pixels(aovs.width, aovs.height);
                let aovs = match crop_beauty_aovs(aovs, rx, ry, rw, rh) {
                    Ok(a) => {
                        if rw < w || rh < h {
                            self.ai.beauty_status = Some(format!(
                                "Region {rw}\u{00d7}{rh} (from {w}\u{00d7}{h}) \u{2014} sending to AI\u{2026}"
                            ));
                        }
                        a
                    }
                    Err(e) => {
                        self.ai.beauty_status = Some(format!(" Region crop failed: {e}"));
                        self.ai.beauty_busy = false;
                        self.end_ai_gpu_exclusive_if_idle();
                        return;
                    }
                };

                // Cache source beauty for before/after + overlay blend
                self.ai.beauty_source_rgba = aovs.beauty_rgba.clone();
                self.ai.beauty_source_w = aovs.width;
                self.ai.beauty_source_h = aovs.height;
                if let Some(img) = image::RgbaImage::from_raw(aovs.width, aovs.height, aovs.beauty_rgba.clone()) {
                    let color = egui::ColorImage::from_rgba_unmultiplied(
                        [aovs.width as usize, aovs.height as usize],
                        img.as_raw(),
                    );
                    self.ai.beauty_source_handle =
                        Some(ctx.load_texture("ai_beauty_src", color, Default::default()));
                }

                self.spawn_ai_beauty_worker(aovs);
            }
            Err(e) => {
                self.ai.beauty_status = Some(format!(" AOV decode: {e}"));
                self.ai.beauty_busy = false;
                self.end_ai_gpu_exclusive_if_idle();
            }
        }
    }

    fn spawn_ai_beauty_worker(&mut self, aovs: BeautyAovs) {
        self.ai.beauty_generation = self.ai.beauty_generation.wrapping_add(1);
        let gen = self.ai.beauty_generation;
        let abort = Arc::new(AtomicBool::new(false));
        self.ai.beauty_abort = Some(abort.clone());
        self.ai.beauty_busy = true;
        self.begin_ai_gpu_exclusive("Neural Beauty / Flux");
        self.ai.beauty_status = Some("AI exclusive GPU — Flux Neural Beauty…".into());

        let req = NeuralBeautyRequest {
            host: self.ai.ai_tex_host.clone(),
            port: self.ai.ai_tex_port,
            prompt: self.ai.beauty_prompt.clone(),
            strength: self.ai.beauty_strength,
            structure: self.ai.beauty_structure,
            seed: self.ai.beauty_seed,
            steps: self.ai.beauty_steps,
            guidance: self.ai.beauty_guidance,
            max_side: self.ai.beauty_max_side,
            quality_preset: self.ai.beauty_quality.clone(),
            use_depth: self.ai.beauty_use_depth,
            use_normal: self.ai.beauty_use_normal,
            use_edges: self.ai.beauty_use_edges,
            use_curvature: self.ai.beauty_use_curvature,
            beauty_png: aovs.beauty_png,
            depth_png: aovs.depth_png,
            normal_png: aovs.normal_png,
            edges_png: aovs.edges_png,
            curvature_png: aovs.curvature_png,
        };

        let (tx, rx) = std::sync::mpsc::channel();
        self.ai.beauty_rx = Some(rx);

        std::thread::Builder::new()
            .name("ai-beauty".into())
            .spawn(move || {
                if abort.load(std::sync::atomic::Ordering::Relaxed) {
                    let _ = tx.send((gen, Err("Aborted".into())));
                    return;
                }
                let result = request_neural_beauty(&req);
                let _ = tx.send((gen, result));
            })
            .ok();
    }

    pub(crate) fn poll_ai_beauty_result(&mut self, ctx: &egui::Context) {
        if self.ai.beauty_rx.is_some() {
            ctx.request_repaint();
        }
        let got = self.ai.beauty_rx.as_ref().and_then(|rx| rx.try_recv().ok());
        if let Some((gen, result)) = got {
            if gen < self.ai.beauty_generation {
                return; // stale
            }
            self.ai.beauty_rx = None;
            self.ai.beauty_abort = None;
            self.ai.beauty_busy = false;
            self.end_ai_gpu_exclusive_if_idle();
            match result {
                Ok(res) => {
                    let rgba: Vec<u8> = res
                        .pixels
                        .iter()
                        .flat_map(|p| p.to_le_bytes())
                        .collect();
                    let color = egui::ColorImage::from_rgba_unmultiplied(
                        [res.width as usize, res.height as usize],
                        &rgba,
                    );
                    self.ai.beauty_result_handle =
                        Some(ctx.load_texture("ai_beauty_result", color, Default::default()));
                    self.ai.beauty_result_png = res.png_bytes;
                    self.ai.beauty_result_w = res.width;
                    self.ai.beauty_result_h = res.height;
                    self.ai.beauty_overlay_enabled = true;
                    self.ai.beauty_status = Some(format!(
                        " Neural beauty {}Ã—{} in {:.1}s ({})",
                        res.width, res.height, res.elapsed, res.mode
                    ));
                    self.toast(&format!(
                        " AI Beauty ready ({:.1}s)",
                        res.elapsed
                    ));
                }
                Err(e) => {
                    self.ai.beauty_status = Some(format!(" {e}"));
                    self.toast(&format!("AI Beauty failed: {}", &e[..e.len().min(80)]));
                }
            }
        }
    }

    pub(crate) fn draw_ai_beauty_window(&mut self, ctx: &egui::Context) {
        if !self.ai.show_beauty_window {
            return;
        }

        let mut do_generate = false;
        let mut do_stop = false;
        let mut do_save = false;
        let mut do_clear = false;
        let mut toast_msg: Option<String> = None;
        let ai_exclusive = self.ai.ai_gpu_exclusive();

        egui::Window::new("AI Beauty - Path-Tracer Overlay")
            .collapsible(true)
            .resizable(true)
            .default_width(480.0)
            .default_height(640.0)
            .open(&mut self.ai.show_beauty_window)
            .show(ctx, |ui| {
                ui.heading("Neural Beauty Pass");
                ui.weak(
                    "Path-tracer / viewport beauty + depth/normal/edges polish. \
                     Primary model: Flux.1 Kontext. While running, AI owns the GPU (PT paused).",
                );
                if ai_exclusive {
                    ui.colored_label(
                        egui::Color32::from_rgb(255, 180, 60),
                        "AI exclusive — Path Trace / CUDA paused",
                    );
                }
                ui.add_space(6.0);

                ui.horizontal(|ui| {
                    ui.label("Pipeline:");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.ai.ai_tex_host).desired_width(100.0),
                    );
                    ui.label(":");
                    ui.add(egui::DragValue::new(&mut self.ai.ai_tex_port).speed(1));
                    ui.weak("(Azra AI worker)");
                });
                // Quick model switch (shared pipeline)
                ui.horizontal(|ui| {
                    ui.label("Model (local):");
                    for (key, label) in [
                        ("flux-kontext", "Flux.1 Kontext \u{2605}"),
                        ("cyberrealistic-xl", "CyberRealistic XL"),
                        ("juggernaut-xl", "Juggernaut XL"),
                        ("fiamix-xl", "FiaMix XL"),
                    ] {
                        if ui
                            .selectable_label(self.ai.ai_tex_model == key, label)
                            .on_hover_text("Load from ComfyUI models — Flux Kontext first")
                            .clicked()
                            && self.ai.ai_tex_model != key
                        {
                            self.ai.ai_tex_model = key.to_string();
                            let host = self.ai.ai_tex_host.clone();
                            let port = self.ai.ai_tex_port;
                            let mk = key.to_string();
                            self.ai.beauty_status =
                                Some(format!("Loading local model {label}\u{2026}"));
                            std::thread::spawn(move || {
                                let _ = AzraApp::request_switch_model(&host, port, &mk);
                            });
                        }
                    }
                });

                ui.add_space(4.0);
                ui.label("Scene / look prompt");
                ui.add(
                    egui::TextEdit::multiline(&mut self.ai.beauty_prompt)
                        .desired_width(f32::INFINITY)
                        .desired_rows(3),
                );

                ui.add_space(4.0);
                // Quality presets — higher res + steps for less mush / better detail.
                ui.horizontal(|ui| {
                    ui.label("Quality:");
                    for (key, label, hint, side, steps, stren, stru) in &[
                        ("preview",  "Preview",  "768px · 16 steps · fast", 768u32, 16u32, 0.42f32, 0.78f32),
                        ("standard", "Standard", "1024px · 28 steps", 1024, 28, 0.40, 0.80),
                        ("final",    "Final",    "1280px · 36 steps · recommended", 1280, 36, 0.38, 0.82),
                        ("ultra",    "Ultra",    "1536px · 48 steps · slow", 1536, 48, 0.36, 0.85),
                    ] {
                        if ui
                            .selectable_label(self.ai.beauty_quality == *key, *label)
                            .on_hover_text(*hint)
                            .clicked()
                        {
                            self.ai.beauty_quality = key.to_string();
                            self.ai.beauty_max_side = *side;
                            self.ai.beauty_steps = *steps;
                            self.ai.beauty_strength = *stren;
                            self.ai.beauty_structure = *stru;
                        }
                    }
                });
                ui.weak("Uses the same SD model as AI Texture (DreamShaper / Realistic Vision). Prefer Realistic Vision for hard-surface.");

                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.add(
                        egui::Slider::new(&mut self.ai.beauty_strength, 0.15..=0.75)
                            .text("Creative strength"),
                    );
                });
                ui.horizontal(|ui| {
                    ui.add(
                        egui::Slider::new(&mut self.ai.beauty_structure, 0.0..=1.0)
                            .text("Structure lock"),
                    );
                });
                ui.horizontal(|ui| {
                    ui.add(egui::Slider::new(&mut self.ai.beauty_overlay_opacity, 0.0..=1.0).text("Overlay opacity"));
                });

                ui.collapsing("AOVs used", |ui| {
                    ui.checkbox(&mut self.ai.beauty_use_depth, "Depth (from gbuf_normal.w)");
                    ui.checkbox(&mut self.ai.beauty_use_normal, "Normals");
                    ui.checkbox(&mut self.ai.beauty_use_edges, "Silhouette / edges");
                    ui.checkbox(&mut self.ai.beauty_use_curvature, "Curvature");
                });

                ui.collapsing("Advanced", |ui| {
                    ui.horizontal(|ui| {
                        ui.label("Seed:");
                        ui.add(egui::DragValue::new(&mut self.ai.beauty_seed).speed(1));
                        if ui.button("\u{1f3b2}").clicked() {
                            self.ai.beauty_seed = (std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_millis()
                                & 0xFFFFFFFF) as u32;
                        }
                    });
                    ui.add(egui::Slider::new(&mut self.ai.beauty_steps, 8..=50).text("Steps"));
                    ui.add(
                        egui::Slider::new(&mut self.ai.beauty_guidance, 1.0..=12.0).text("CFG"),
                    );
                    ui.horizontal(|ui| {
                        ui.label("Max side:");
                        for &s in &[512u32, 768, 1024, 1280, 1536] {
                            if ui
                                .selectable_label(self.ai.beauty_max_side == s, format!("{s}"))
                                .clicked()
                            {
                                self.ai.beauty_max_side = s;
                            }
                        }
                    });
                });

                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    let busy = self.ai.beauty_busy
                        || self.ai.beauty_capture_requested
                        || self.ai.pending_beauty_capture.is_some()
                        || self.ai.beauty_rx.is_some();
                    ui.add_enabled_ui(!busy, |ui| {
                        if ui
                            .button(egui::RichText::new("Generate AI Beauty").strong())
                            .on_hover_text("Capture current viewport AOVs and run neural polish")
                            .clicked()
                        {
                            do_generate = true;
                        }
                    });
                    if busy {
                        ui.spinner();
                        if ui.add(
                            egui::Button::new(
                                egui::RichText::new("Stop").color(egui::Color32::from_rgb(255, 120, 100)),
                            ),
                        )
                        .on_hover_text("Cancel beauty job")
                        .clicked()
                        {
                            do_stop = true;
                        }
                    }
                    if ui.button("Save PNG").clicked() {
                        do_save = true;
                    }
                    if ui.button("Clear")
                        .on_hover_text("Stop if running, then clear result")
                        .clicked()
                    {
                        do_clear = true;
                    }
                });

                ui.checkbox(&mut self.ai.beauty_overlay_enabled, "Show as viewport overlay");
                ui.checkbox(&mut self.ai.beauty_region_overlay, "Fit overlay into render region");
                ui.checkbox(&mut self.ai.beauty_show_split, "Split before | after");

                // ── Render region (crop for resolution efficiency) ──
                ui.add_space(4.0);
                ui.separator();
                ui.strong("Render region");
                ui.weak(
                    "Draw a rectangle on the viewport so AI spends resolution on the subject only \
                     (like Blender crop). Full frame if empty.",
                );
                ui.horizontal(|ui| {
                    let drawing = self.ai.beauty_region_draw;
                    let label = if drawing {
                        "Drawing region\u{2026} (LMB drag on viewport)"
                    } else {
                        "Draw region on viewport"
                    };
                    if ui
                        .selectable_label(drawing, label)
                        .on_hover_text("Click, then LMB-drag a box on the 3D view. Esc cancels draw mode.")
                        .clicked()
                    {
                        self.ai.beauty_region_draw = !drawing;
                        if self.ai.beauty_region_draw {
                            self.ai.beauty_region_drag_start = None;
                            self.ai.beauty_region_drag_cur = None;
                            toast_msg = Some("AI region: LMB-drag on viewport to set crop".into());
                        }
                    }
                    if ui.button("Clear region").clicked() {
                        self.ai.beauty_region_uv = None;
                        self.ai.beauty_region_draw = false;
                        self.ai.beauty_region_drag_start = None;
                        self.ai.beauty_region_drag_cur = None;
                        toast_msg = Some("AI region cleared — full frame".into());
                    }
                });
                if let Some([u0, v0, u1, v1]) = self.ai.beauty_region_uv {
                    let uw = ((u0 - u1).abs() * 100.0) as u32;
                    let uh = ((v0 - v1).abs() * 100.0) as u32;
                    ui.colored_label(
                        egui::Color32::from_rgb(120, 200, 255),
                        format!(
                            "Region active: ~{uw}% \u{00d7} {uh}% of viewport  \u{00b7}  \
                             max_side={} applies to this crop",
                            self.ai.beauty_max_side
                        ),
                    );
                } else {
                    ui.weak("No region \u{2014} full viewport will be sent to AI");
                }

                if let Some(ref st) = self.ai.beauty_status {
                    ui.add_space(4.0);
                    ui.label(st);
                }

                // Preview
                ui.add_space(6.0);
                ui.separator();
                let avail = ui.available_width();
                if self.ai.beauty_show_split {
                    ui.horizontal(|ui| {
                        let half = (avail - 8.0) * 0.5;
                        if let Some(ref h) = self.ai.beauty_source_handle {
                            ui.vertical(|ui| {
                                ui.weak("Before (PT/viewport)");
                                ui.image((h.id(), egui::vec2(half, half * 0.6)));
                            });
                        }
                        if let Some(ref h) = self.ai.beauty_result_handle {
                            ui.vertical(|ui| {
                                ui.weak("After (AI Beauty)");
                                ui.image((h.id(), egui::vec2(half, half * 0.6)));
                            });
                        }
                    });
                } else if let Some(ref h) = self.ai.beauty_result_handle {
                    let aspect = if self.ai.beauty_result_h > 0 {
                        self.ai.beauty_result_w as f32 / self.ai.beauty_result_h as f32
                    } else {
                        1.6
                    };
                    let w = avail.min(640.0);
                    let ht = w / aspect;
                    ui.image((h.id(), egui::vec2(w, ht)));
                } else if let Some(ref h) = self.ai.beauty_source_handle {
                    ui.weak("Source captured  -  generate to polish");
                    ui.image((h.id(), egui::vec2(avail.min(480.0), 200.0)));
                } else {
                    ui.weak("No frame yet. Frame your shot, then Generate.");
                }
            });

        if let Some(msg) = toast_msg {
            self.toast(&msg);
        }
        if do_stop {
            self.stop_ai_beauty_job("user");
            self.toast("Beauty stopped");
        }
        if do_generate {
            self.request_ai_beauty_capture();
        }
        if do_save {
            if self.ai.beauty_result_png.is_empty() {
                self.toast("Nothing to save  -  generate first");
            } else if let Some(path) = rfd::FileDialog::new()
                .set_title("Save AI Beauty")
                .add_filter("PNG", &["png"])
                .save_file()
            {
                match std::fs::write(&path, &self.ai.beauty_result_png) {
                    Ok(_) => self.toast(&format!("Saved {}", path.display())),
                    Err(e) => self.toast(&format!("Save failed: {e}")),
                }
            }
        }
        if do_clear {
            self.clear_ai_beauty();
            self.toast("Beauty cleared");
        }
    }

    /// Convert committed UV region (or live drag) to an absolute screen rect in the viewport.
    pub(crate) fn ai_render_region_screen_rect(&self, viewport: egui::Rect) -> Option<egui::Rect> {
        if let (Some(a), Some(b)) = (
            self.ai.beauty_region_drag_start,
            self.ai.beauty_region_drag_cur,
        ) {
            let a = viewport.min + a.to_vec2();
            let b = viewport.min + b.to_vec2();
            return Some(egui::Rect::from_two_pos(a, b));
        }
        let [u0, v0, u1, v1] = self.ai.beauty_region_uv?;
        let x0 = viewport.min.x + u0.min(u1) * viewport.width();
        let y0 = viewport.min.y + v0.min(v1) * viewport.height();
        let x1 = viewport.min.x + u0.max(u1) * viewport.width();
        let y1 = viewport.min.y + v0.max(v1) * viewport.height();
        Some(egui::Rect::from_min_max(egui::pos2(x0, y0), egui::pos2(x1, y1)))
    }

    /// Draw render-region marquee + AI beauty overlay on the central viewport panel.
    pub(crate) fn draw_ai_beauty_viewport_overlay(&self, ui: &mut egui::Ui, rect: egui::Rect) {
        let painter = ui.painter_at(rect);

        if self.ai.beauty_region_draw || self.ai.beauty_region_uv.is_some() {
            if let Some(rr) = self.ai_render_region_screen_rect(rect) {
                let rr = rr.intersect(rect);
                if rr.width() > 2.0 && rr.height() > 2.0 {
                    let dim = egui::Color32::from_black_alpha(90);
                    let full = rect;
                    painter.rect_filled(
                        egui::Rect::from_min_max(full.min, egui::pos2(full.max.x, rr.min.y)),
                        0.0,
                        dim,
                    );
                    painter.rect_filled(
                        egui::Rect::from_min_max(egui::pos2(full.min.x, rr.max.y), full.max),
                        0.0,
                        dim,
                    );
                    painter.rect_filled(
                        egui::Rect::from_min_max(
                            egui::pos2(full.min.x, rr.min.y),
                            egui::pos2(rr.min.x, rr.max.y),
                        ),
                        0.0,
                        dim,
                    );
                    painter.rect_filled(
                        egui::Rect::from_min_max(
                            egui::pos2(rr.max.x, rr.min.y),
                            egui::pos2(full.max.x, rr.max.y),
                        ),
                        0.0,
                        dim,
                    );
                    let col = if self.ai.beauty_region_draw {
                        egui::Color32::from_rgb(255, 200, 80)
                    } else {
                        egui::Color32::from_rgb(80, 180, 255)
                    };
                    painter.rect_stroke(rr, 0.0, egui::Stroke::new(2.0, col), egui::StrokeKind::Middle);
                    let t = 10.0;
                    for (c, dx, dy) in [
                        (rr.left_top(), 1.0, 1.0),
                        (rr.right_top(), -1.0, 1.0),
                        (rr.left_bottom(), 1.0, -1.0),
                        (rr.right_bottom(), -1.0, -1.0),
                    ] {
                        painter.line_segment(
                            [c, egui::pos2(c.x + dx * t, c.y)],
                            egui::Stroke::new(2.5, col),
                        );
                        painter.line_segment(
                            [c, egui::pos2(c.x, c.y + dy * t)],
                            egui::Stroke::new(2.5, col),
                        );
                    }
                    painter.text(
                        rr.left_top() + egui::vec2(4.0, 4.0),
                        egui::Align2::LEFT_TOP,
                        format!("{}x{}", rr.width() as i32, rr.height() as i32),
                        egui::FontId::proportional(12.0),
                        col,
                    );
                }
            }
        }

        if !self.ai.beauty_overlay_enabled {
            return;
        }
        let Some(ref handle) = self.ai.beauty_result_handle else {
            return;
        };
        let opacity = self.ai.beauty_overlay_opacity.clamp(0.0, 1.0);
        if opacity <= 0.01 {
            return;
        }
        let iw = self.ai.beauty_result_w.max(1) as f32;
        let ih = self.ai.beauty_result_h.max(1) as f32;
        let img_rect = if self.ai.beauty_region_overlay {
            if let Some(rr) = self.ai_render_region_screen_rect(rect) {
                rr.intersect(rect)
            } else {
                let scale = (rect.width() / iw).min(rect.height() / ih);
                egui::Rect::from_center_size(rect.center(), egui::vec2(iw * scale, ih * scale))
            }
        } else {
            let scale = (rect.width() / iw).min(rect.height() / ih);
            egui::Rect::from_center_size(rect.center(), egui::vec2(iw * scale, ih * scale))
        };
        if img_rect.width() < 2.0 || img_rect.height() < 2.0 {
            return;
        }
        let tint = egui::Color32::from_white_alpha((opacity * 255.0) as u8);
        egui::Image::new(egui::load::SizedTexture::new(
            handle.id(),
            egui::vec2(img_rect.width(), img_rect.height()),
        ))
        .tint(tint)
        .paint_at(ui, img_rect);
    }
}
