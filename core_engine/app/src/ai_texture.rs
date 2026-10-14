// core_engine/app/src/ai_texture.rs
// AI Texture generation HTTP methods and helpers, extracted from main.rs.

use eframe::egui;
use eframe::wgpu;
use super::{AzraApp, DirtyFlags};

// ---- AI Texture — result type ----
// Returned by request_ai_texture; carries everything needed to update state.
// map_pngs: full-res PNG bytes for disk save ([albedo, normal, roughness, metalness, height])
// map_thumbs: 128×128 RGBA packed for preview thumbnails (same order)
pub(crate) struct AiTexGenResult {
    pub(crate) pixels:     Vec<u32>,        // full-res RGBA for GPU (albedo)
    pub(crate) dim:        u32,             // texture dimension (square)
    pub(crate) map_pngs:   [Vec<u8>; 5],   // full-res PNG bytes [albedo, normal, roughness, metalness, height]
    pub(crate) map_thumbs: [Vec<u32>; 5],  // 128×128 RGBA packed thumbnails (same order)
    pub(crate) gen_id:     Option<u32>,    // SQLite generation id (for rating)
    pub(crate) db_total:   u32,
    pub(crate) db_liked:   u32,
}

impl AzraApp {
    // ---- JSON string escaping — handles backslash, quote, newline, control chars ----
    pub(crate) fn json_escape(s: &str) -> String {
        let mut out = String::with_capacity(s.len() + 8);
        for c in s.chars() {
            match c {
                '"'  => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                c if (c as u32) < 0x20 => { out.push_str(&format!("\\u{:04x}", c as u32)); }
                c    => out.push(c),
            }
        }
        out
    }

    pub(crate) fn request_ai_texture(host: &str, port: u16, prompt: &str, size: u32, seed: u32, steps: u32, guidance: f32, quality: &str) -> Result<AiTexGenResult, String> {
        use std::io::{Read, Write};
        use std::net::TcpStream;
        let addr = format!("{}:{}", host, port);
        let sock_addr: std::net::SocketAddr = addr.parse().map_err(|e| format!("Bad address: {}", e))?;
        let mut stream = TcpStream::connect_timeout(&sock_addr, std::time::Duration::from_secs(5))
            .map_err(|e| format!("Cannot connect to {} — server may still be starting up, retry in a few seconds ({})", addr, e))?;
        // Flux.1 Kontext can take several minutes (load + denoise at 1024–1536).
        stream.set_read_timeout(Some(std::time::Duration::from_secs(600))).ok();

        let body = format!(
            r#"{{"prompt":"{}","width":{},"height":{},"seed":{},"steps":{},"guidance_scale":{},"quality_preset":"{}"}}"#,
            Self::json_escape(prompt), size, size, seed, steps, guidance, quality
        );

        let request = format!(
            "POST /generate HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            addr, body.len(), body
        );
        stream.write_all(request.as_bytes()).map_err(|e| format!("Write: {}", e))?;

        let mut response_bytes = Vec::new();
        stream.read_to_end(&mut response_bytes).map_err(|e| format!("Read: {}", e))?;
        let response = String::from_utf8_lossy(&response_bytes).to_string();

        let body_start = response.find("\r\n\r\n").ok_or("No HTTP headers")?;
        let http_body = &response[body_start + 4..];

        if response.contains("chunked") {
            let decoded = Self::decode_chunked(http_body)?;
            return Self::parse_texture_response(&decoded, size);
        }
        Self::parse_texture_response(http_body, size)
    }

    /// POST /rate — record user rating (1=bad 2=ok 3=good 4=perfect)
    pub(crate) fn request_rate(host: &str, port: u16, gen_id: u32, rating: u32) {
        use std::io::{Read, Write};
        use std::net::TcpStream;
        let addr = format!("{}:{}", host, port);
        if let Ok(sock) = addr.parse::<std::net::SocketAddr>() {
            if let Ok(mut s) = TcpStream::connect_timeout(&sock, std::time::Duration::from_secs(3)) {
                s.set_read_timeout(Some(std::time::Duration::from_secs(10))).ok();
                let body = format!(r#"{{"gen_id":{},"rating":{}}}"#, gen_id, rating);
                let req = format!("POST /rate HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", addr, body.len(), body);
                let _ = s.write_all(req.as_bytes());
                let mut buf = Vec::new();
                let _ = s.read_to_end(&mut buf);
            }
        }
    }

    /// POST /switch_model — blocking (model load can take 30-60s)
    pub(crate) fn request_switch_model(host: &str, port: u16, model_key: &str) -> Result<String, String> {
        use std::io::{Read, Write};
        use std::net::TcpStream;
        let addr = format!("{}:{}", host, port);
        let sock_addr: std::net::SocketAddr = addr.parse().map_err(|e| format!("Bad address: {}", e))?;
        let mut stream = TcpStream::connect_timeout(&sock_addr, std::time::Duration::from_secs(5))
            .map_err(|e| format!("Cannot connect: {}", e))?;
        // Model swap for Flux can exceed 2 minutes (22GB transformer + offload).
        stream.set_read_timeout(Some(std::time::Duration::from_secs(600))).ok();
        let body = format!(r#"{{"model_key":"{}"}}"#, model_key);
        let req = format!("POST /switch_model HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", addr, body.len(), body);
        stream.write_all(req.as_bytes()).map_err(|e| format!("Write: {}", e))?;
        let mut resp_bytes = Vec::new();
        stream.read_to_end(&mut resp_bytes).map_err(|e| format!("Read: {}", e))?;
        let resp = String::from_utf8_lossy(&resp_bytes).to_string();
        let bs = resp.find("\r\n\r\n").unwrap_or(0);
        if resp.len() < bs + 4 { return Err("Malformed HTTP response".into()); }
        let http_body = &resp[bs + 4..];
        if http_body.contains("\"error\"") {
            return Err(format!("Server: {}", &http_body[..http_body.len().min(200)]));
        }
        // Extract label from response
        let label = if let Some(s) = http_body.find("\"label\":\"") {
            let ds = s + 9;
            Self::extract_json_string_escaped(&http_body[ds..]).unwrap_or_else(|| model_key.to_string())
        } else { model_key.to_string() };
        Ok(label)
    }

    /// Minimal base64 encoder — no external crate needed.
    pub(crate) fn encode_base64(bytes: &[u8]) -> String {
        const C: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::with_capacity((bytes.len() + 2) / 3 * 4);
        for chunk in bytes.chunks(3) {
            let b0 = chunk[0] as u32;
            let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
            let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
            let n = (b0 << 16) | (b1 << 8) | b2;
            out.push(C[((n >> 18) & 63) as usize] as char);
            out.push(C[((n >> 12) & 63) as usize] as char);
            if chunk.len() > 1 { out.push(C[((n >> 6) & 63) as usize] as char); } else { out.push('='); }
            if chunk.len() > 2 { out.push(C[(n & 63) as usize] as char); }         else { out.push('='); }
        }
        out
    }

    /// POST /enhance_prompt — rewrite a short material description with Ollama.
    /// Returns the enhanced string, or the original on failure / server unavailable.
    pub(crate) fn request_enhance_prompt(host: &str, port: u16, prompt: &str, model: &str) -> Result<String, String> {
        use std::io::{Read, Write};
        use std::net::TcpStream;
        let addr = format!("{}:{}", host, port);
        let sock: std::net::SocketAddr = addr.parse().map_err(|e| format!("{}", e))?;
        let mut s = TcpStream::connect_timeout(&sock, std::time::Duration::from_secs(5))
            .map_err(|e| format!("Cannot connect: {}", e))?;
        s.set_read_timeout(Some(std::time::Duration::from_secs(45))).ok();
        let body = format!(r#"{{"prompt":"{}","model":"{}"}}"#, Self::json_escape(prompt), Self::json_escape(model));
        let req = format!(
            "POST /enhance_prompt HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            addr, body.len(), body
        );
        s.write_all(req.as_bytes()).map_err(|e| format!("Write: {}", e))?;
        let mut resp = Vec::new();
        s.read_to_end(&mut resp).map_err(|e| format!("Read: {}", e))?;
        let text = String::from_utf8_lossy(&resp).to_string();
        let bs = text.find("\r\n\r\n").unwrap_or(0);
        if text.len() < bs + 4 { return Err("Malformed HTTP response".into()); }
        let body_start = bs + 4;
        let http_body = &text[body_start..];
        // Chunked transfer decoding if needed
        let http_body = if text.contains("Transfer-Encoding: chunked") {
            Self::decode_chunked(http_body).unwrap_or_else(|_| http_body.to_string())
        } else { http_body.to_string() };
        // Check specific failures with clean messages before raw JSON dump
        if http_body.contains("ollama_unavailable") {
            return Err("Ollama not running — install: ollama pull llama3.2:3b".into());
        }
        if http_body.contains("\"error\"") {
            return Err(format!("Server: {}", &http_body[..http_body.len().min(120)]));
        }
        // Extract "enhanced" field (escape-aware)
        if let Some(s) = http_body.find("\"enhanced\":\"") {
            let ds = s + 12;
            if let Some(val) = Self::extract_json_string_escaped(&http_body[ds..]) {
                return Ok(val);
            }
        }
        Err("No enhanced field in response".into())
    }

    /// POST /describe_image — have Ollama LLaVA describe a material reference photo.
    /// Returns (description, enhanced_prompt) or an error.
    pub(crate) fn request_describe_image(host: &str, port: u16, image_b64: &str) -> Result<(String, String), String> {
        use std::io::{Read, Write};
        use std::net::TcpStream;
        let addr = format!("{}:{}", host, port);
        let sock: std::net::SocketAddr = addr.parse().map_err(|e| format!("{}", e))?;
        let mut s = TcpStream::connect_timeout(&sock, std::time::Duration::from_secs(5))
            .map_err(|e| format!("Cannot connect: {}", e))?;
        s.set_read_timeout(Some(std::time::Duration::from_secs(90))).ok(); // vision models are slow
        let body = format!(r#"{{"image_b64":"{}","auto_enhance":true}}"#, image_b64);
        let req = format!(
            "POST /describe_image HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            addr, body.len(), body
        );
        s.write_all(req.as_bytes()).map_err(|e| format!("Write: {}", e))?;
        let mut resp = Vec::new();
        s.read_to_end(&mut resp).map_err(|e| format!("Read: {}", e))?;
        let text = String::from_utf8_lossy(&resp).to_string();
        let bs = text.find("\r\n\r\n").unwrap_or(0);
        if text.len() < bs + 4 { return Err("Malformed HTTP response".into()); }
        let body_start = bs + 4;
        let http_body = &text[body_start..];
        let http_body = if text.contains("Transfer-Encoding: chunked") {
            Self::decode_chunked(http_body).unwrap_or_else(|_| http_body.to_string())
        } else { http_body.to_string() };
        if http_body.contains("\"error\"") {
            return Err(format!("Ollama not available. Run: ollama pull llava:7b"));
        }
        let extract = |key: &str| -> String {
            let needle = format!("\"{}\":\"", key);
            if let Some(s) = http_body.find(&needle) {
                let ds = s + needle.len();
                Self::extract_json_string_escaped(&http_body[ds..]).unwrap_or_default()
            } else {
                String::new()
            }
        };
        let description = extract("description");
        let enhanced    = extract("enhanced");
        if description.is_empty() {
            return Err("Empty response — is ollama running? (ollama serve)".into());
        }
        let fallback = description.clone();
        Ok((description, if enhanced.is_empty() { fallback } else { enhanced }))
    }

    pub(crate) fn decode_chunked(raw: &str) -> Result<String, String> {
        let mut result = String::new();
        let mut pos = 0;
        let bytes = raw.as_bytes();
        while pos < bytes.len() {
            // Read chunk size (hex) until \r\n
            let line_end = raw[pos..].find("\r\n").unwrap_or(raw.len() - pos);
            let hex = raw[pos..pos + line_end].trim();
            let chunk_size = usize::from_str_radix(hex, 16).unwrap_or(0);
            if chunk_size == 0 { break; }
            pos += line_end + 2; // skip hex + \r\n
            if pos + chunk_size <= raw.len() {
                result.push_str(&raw[pos..pos + chunk_size]);
            }
            pos += chunk_size + 2; // skip chunk data + \r\n
        }
        Ok(result)
    }

    pub(crate) fn parse_texture_response(http_body: &str, size: u32) -> Result<AiTexGenResult, String> {
        if http_body.contains("\"error\"") {
            let es = http_body.find("\"error\":\"").unwrap_or(0);
            return Err(format!("Server: {}", &http_body[es..http_body.len().min(es+200)]));
        }
        let dim = Self::extract_json_u32(http_body, "width").unwrap_or(size);

        // Helper: extract a base64 value by JSON key → decoded bytes
        let extract_b64 = |key: &str| -> Vec<u8> {
            let full_key = format!("\"{}\":\"", key);
            if let Some(start) = http_body.find(&full_key) {
                let ds = start + full_key.len();
                if let Some(end) = http_body[ds..].find('"') {
                    if let Ok(bytes) = Self::decode_base64(&http_body[ds..ds+end]) {
                        return bytes;
                    }
                }
            }
            Vec::new()
        };

        // Extract albedo RGBA for GPU (full-res)
        let albedo_raw = extract_b64("albedo_rgba_b64");
        if albedo_raw.is_empty() {
            return Err(format!("No albedo_rgba_b64 (body: {})", &http_body[..http_body.len().min(300)]));
        }
        let pc = (dim * dim) as usize;
        if albedo_raw.len() < pc * 4 {
            return Err(format!("Decoded {} bytes, need {}", albedo_raw.len(), pc * 4));
        }
        let mut pixels = Vec::with_capacity(pc);
        for i in 0..pc {
            let o = i * 4;
            // Pack as (R<<24)|(G<<16)|(B<<8)|A so that:
            //   GPU tex_fetch: (packed>>24)=R, (packed>>16)=G, (packed>>8)=B  ✓
            //   register_map_preview: (pk>>24)=R, (pk>>16)=G, (pk>>8)=B       ✓
            pixels.push(u32::from_be_bytes([albedo_raw[o], albedo_raw[o+1], albedo_raw[o+2], albedo_raw[o+3]]));
        }

        // Decode 128×128 RGBA thumbnail for each map
        let thumb_keys  = ["albedo_thumb_b64",   "normal_thumb_b64",   "roughness_thumb_b64",
                           "metalness_thumb_b64", "height_thumb_b64"];
        let mut map_thumbs: [Vec<u32>; 5] = Default::default();
        const THUMB_DIM: usize = 128;
        for (i, key) in thumb_keys.iter().enumerate() {
            let raw = extract_b64(key);
            let tc = THUMB_DIM * THUMB_DIM;
            if raw.len() >= tc * 4 {
                let mut thumb = Vec::with_capacity(tc);
                for j in 0..tc {
                    let o = j * 4;
                    // Same (R<<24)|(G<<16)|(B<<8)|A packing as the full-res albedo above.
                    thumb.push(u32::from_be_bytes([raw[o], raw[o+1], raw[o+2], raw[o+3]]));
                }
                map_thumbs[i] = thumb;
            }
        }

        // Full-res PNGs for saving
        let png_keys = ["albedo_png_b64", "normal_png_b64", "roughness_png_b64",
                        "metalness_png_b64", "height_png_b64"];
        let mut map_pngs: [Vec<u8>; 5] = Default::default();
        for (i, key) in png_keys.iter().enumerate() {
            map_pngs[i] = extract_b64(key);
        }

        // DB metadata
        let gen_id = Self::extract_json_u32(http_body, "gen_id");
        let db_total = if let Some(p) = http_body.find("\"total\":") {
            let s = p + 8;
            let tr = http_body[s..].trim_start();
            let e = tr.find(|c: char| !c.is_ascii_digit()).unwrap_or(tr.len());
            tr[..e].parse().unwrap_or(0)
        } else { 0 };
        let db_liked = if let Some(p) = http_body.find("\"liked\":") {
            let s = p + 8;
            let tr = http_body[s..].trim_start();
            let e = tr.find(|c: char| !c.is_ascii_digit()).unwrap_or(tr.len());
            tr[..e].parse().unwrap_or(0)
        } else { 0 };

        Ok(AiTexGenResult { pixels, dim, map_pngs, map_thumbs, gen_id, db_total, db_liked })
    }

    /// Escape-aware JSON string extraction.
    /// Input must start AFTER the opening quote of the value.
    /// Scans char-by-char, correctly handling \" sequences.
    /// Returns the unescaped string content up to the closing quote.
    fn extract_json_string_escaped(s: &str) -> Option<String> {
        let mut result = String::new();
        let mut chars = s.chars();
        loop {
            match chars.next() {
                Some('\\') => match chars.next() {
                    Some('n')  => result.push('\n'),
                    Some('t')  => result.push('\t'),
                    Some('r')  => {}
                    Some('"')  => result.push('"'),
                    Some('\\') => result.push('\\'),
                    Some(c)    => { result.push('\\'); result.push(c); }
                    None       => break,
                },
                Some('"') => return Some(result),
                Some(c)   => result.push(c),
                None      => break,
            }
        }
        if result.is_empty() { None } else { Some(result) }
    }

    pub(crate) fn extract_json_u32(json: &str, key: &str) -> Option<u32> {
        let pat = format!("\"{}\":", key);
        let start = json.find(&pat)? + pat.len();
        let trimmed = json[start..].trim_start();
        let end = trimmed.find(|c: char| !c.is_ascii_digit()).unwrap_or(trimmed.len());
        trimmed[..end].parse().ok()
    }

    pub(crate) fn decode_base64(input: &str) -> Result<Vec<u8>, String> {
        let table = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = Vec::with_capacity(input.len() * 3 / 4);
        let mut buf = 0u32;
        let mut bits = 0u32;
        for &ch in input.as_bytes() {
            if ch == b'=' || ch == b'\n' || ch == b'\r' { continue; }
            let val = table.iter().position(|&c| c == ch)
                .ok_or_else(|| format!("Invalid base64 char: {}", ch as char))? as u32;
            buf = (buf << 6) | val;
            bits += 6;
            if bits >= 8 { bits -= 8; out.push((buf >> bits) as u8); buf &= (1 << bits) - 1; }
        }
        Ok(out)
    }


    /// Abort in-flight AI texture job (UI stop). Worker may still finish on the
    /// server; we ignore the result via generation bump + dropped rx.
    pub(crate) fn stop_ai_texture_job(&mut self, reason: &str) {
        if let Some(ref abort) = self.ai.ai_tex_abort {
            abort.store(true, std::sync::atomic::Ordering::SeqCst);
        }
        // Bump generation so any late result is discarded in poll.
        self.ai.ai_tex_generation = self.ai.ai_tex_generation.wrapping_add(1);
        self.ai.ai_tex_rx = None;
        self.ai.ai_tex_abort = None;
        self.ai.ai_tex_status = Some(format!("\u{23f9} {reason}"));
        self.end_ai_gpu_exclusive_if_idle();
    }

    /// Safe clear of AI texture maps + GPU buffer (also stops generation).
    /// Avoids crash when clearing mid-gen or while egui still holds previews.
    pub(crate) fn clear_ai_texture_maps(&mut self) {
        // Always stop first — clear mid-generate was a crash path.
        if self.ai.ai_tex_rx.is_some() || self.ai.ai_tex_abort.is_some() {
            self.stop_ai_texture_job("Stopped (clear)");
        }
        self.ai.ai_tex_textured.clear();
        self.ai.ai_tex_dim = 0;
        self.ai.ai_tex_data.clear();
        self.ai.ai_tex_maps_png = Default::default();
        self.ai.ai_tex_map_thumbs = Default::default();
        self.ai.ai_tex_last_gen_id = None;
        // Defer free of egui textures to next frame (same as register_map_preview).
        for i in 0..5 {
            if let Some(handle) = self.ai.ai_tex_handles[i].take() {
                // If dropping slot still occupied, drop the previous one now (one frame old).
                self.ai.ai_tex_dropping[i] = Some(handle);
            }
        }
        // 1×1 black placeholder so shaders never see a zero-size buffer.
        self.ai.ai_tex_data = vec![0u32];
        self.ai.ai_tex_dim = 1;
        self.upload_ai_texture_to_gpu();
        // Dim 0 = "no texture" for Apply UI; keep 1px GPU buffer.
        self.ai.ai_tex_dim = 0;
        self.ai.ai_tex_data.clear();
        self.cache.prim_buf_stale = true;
        self.mark_dirty(DirtyFlags::RENDER);
    }

    pub(crate) fn upload_ai_texture_to_gpu(&mut self) {
        if let Some(ref mut gpu) = self.gpu {
            let device = gpu.device.clone();
            let queue = gpu.queue.clone();
            {
                // Never create a zero-size buffer (wgpu can panic / device-loss).
                let data_bytes: Vec<u8> = if self.ai.ai_tex_data.is_empty() {
                    vec![0u8; 4] // 1 pixel RGBA packed as u32
                } else {
                    self.ai.ai_tex_data.iter().flat_map(|v| v.to_le_bytes()).collect()
                };
                let size = (data_bytes.len() as u64).max(4);
                gpu.buffers.tex_buffer = device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("TexData"), size,
                    usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
                queue.write_buffer(&gpu.buffers.tex_buffer, 0, &data_bytes);
                // Invalidate all bind groups that reference tex_buffer
                gpu.bind_groups.bind_group = None;
                gpu.bind_groups.deferred_bg = None;
                gpu.bind_groups.pt_bg = None;
                gpu.texture_size = (0, 0);
            }
        }
    }

    pub(crate) const MAP_NAMES: [&'static str; 5] = ["albedo", "normal", "roughness", "metalness", "height"];
    pub(crate) const MAP_LABELS: [&'static str; 5] = ["Albedo", "Normal", "Roughness", "Metalness", "Height"];
    pub(crate) const MAP_ICONS: [&'static str; 5] = ["🎨", "🔵", "⬜", "⚙", "⬆"];

    /// Load a 128×128 RGBA packed thumbnail into an egui texture handle for map index i.
    /// Safe from same-frame free: old handle is moved to ai_tex_dropping (freed next frame).
    pub(crate) fn register_map_preview(&mut self, ctx: &egui::Context, i: usize, thumb: &[u32]) {
        if thumb.len() < 128 * 128 { return; }
        let mut rgba = Vec::with_capacity(128 * 128 * 4);
        for &pk in &thumb[..128 * 128] {
            rgba.push(((pk >> 24) & 0xFF) as u8);
            rgba.push(((pk >> 16) & 0xFF) as u8);
            rgba.push(((pk >>  8) & 0xFF) as u8);
            rgba.push(255u8);
        }
        let image = egui::ColorImage::from_rgba_unmultiplied([128, 128], &rgba);
        let names = ["ai_map_albedo", "ai_map_normal", "ai_map_roughness", "ai_map_metalness", "ai_map_height"];
        self.ai.ai_tex_dropping[i] = self.ai.ai_tex_handles[i].take();
        self.ai.ai_tex_handles[i] = Some(ctx.load_texture(names[i], image, egui::TextureOptions::LINEAR));
    }

    pub(crate) fn save_ai_map(map_idx: usize, png_data: &[u8]) -> Option<String> {
        if png_data.is_empty() { return None; }
        if let Some(path) = rfd::FileDialog::new()
            .set_title(&format!("Save {} Map", Self::MAP_LABELS[map_idx]))
            .add_filter("PNG", &["png"]).add_filter("TGA", &["tga"])
            .set_file_name(&format!("fl_{}.png", Self::MAP_NAMES[map_idx]))
            .save_file()
        {
            let p = path.to_string_lossy().to_string();
            match std::fs::write(&p, png_data) {
                Ok(_) => Some(format!("✅ Saved {}", p)),
                Err(e) => Some(format!("❌ {}", e)),
            }
        } else { None }
    }

    pub(crate) fn load_texture_file(&mut self) -> Option<String> {
        let path = rfd::FileDialog::new()
            .set_title("Load Texture Image")
            .add_filter("Image", &["png","jpg","jpeg","tga","bmp"])
            .pick_file()?;
        let p = path.to_string_lossy().to_string();
        let bytes = std::fs::read(&p).ok()?;
        if p.ends_with(".tga") {
            if let Some((pixels, w, _h)) = Self::load_tga_rgba(&bytes) {
                self.ai.ai_tex_data = pixels;
                self.ai.ai_tex_dim = w;
                self.upload_ai_texture_to_gpu();
                self.mark_dirty(DirtyFlags::RENDER);
                return Some(format!("✅ Loaded {}×{} TGA", w, w));
            }
        }
        self.ai.ai_tex_maps_png[0] = bytes;
        Some(format!("✅ Loaded {}", p))
    }

    pub(crate) fn load_tga_rgba(data: &[u8]) -> Option<(Vec<u32>, u32, u32)> {
        if data.len() < 18 { return None; }
        // data[2] = image type: 2=uncompressed RGB, 3=uncompressed grey, 10/11=RLE variants.
        // We only support uncompressed (type 2/3). Reject RLE silently rather than producing garbage.
        let img_type = data[2];
        if img_type != 2 && img_type != 3 { return None; }
        let w = u16::from_le_bytes([data[12], data[13]]) as u32;
        let h = u16::from_le_bytes([data[14], data[15]]) as u32;
        let bpp = data[16];
        if w == 0 || h == 0 || w > 4096 { return None; }
        let start = 18 + data[0] as usize;
        let mut px = Vec::with_capacity((w*h) as usize);
        for i in 0..(w*h) as usize {
            let off = start + i * (bpp as usize / 8);
            if off + 2 >= data.len() { break; }
            let (b, g, r) = (data[off], data[off+1], data[off+2]);
            px.push((r as u32)<<24 | (g as u32)<<16 | (b as u32)<<8 | 255);
        }
        Some((px, w, h))
    }

    /// Parse Radiance RGBE (.hdr) file → Vec<f32> of RGB triplets + (width, height)
    pub(crate) fn parse_hdr(data: &[u8]) -> Option<(Vec<f32>, u32, u32)> {
        // Skip header lines until blank line
        let mut pos = 0;
        loop {
            if pos >= data.len() { return None; }
            let line_start = pos;
            while pos < data.len() && data[pos] != b'\n' { pos += 1; }
            let line = &data[line_start..pos];
            pos += 1; // skip \n
            if line.is_empty() || (line.len() == 1 && line[0] == b'\r') { break; }
        }
        // Parse resolution line: "-Y height +X width"
        if pos >= data.len() { return None; }
        let res_start = pos;
        while pos < data.len() && data[pos] != b'\n' { pos += 1; }
        let res_line = std::str::from_utf8(&data[res_start..pos]).ok()?;
        pos += 1;
        let parts: Vec<&str> = res_line.split_whitespace().collect();
        if parts.len() < 4 { return None; }
        let height: u32 = parts[1].parse().ok()?;
        let width: u32 = parts[3].parse().ok()?;
        if width == 0 || height == 0 || width > 16384 || height > 16384 { return None; }
        let pixel_count = (width * height) as usize;
        let mut pixels = vec![0.0f32; pixel_count * 3];
        // Decode RGBE scanlines
        let mut y = 0usize;
        while y < height as usize && pos < data.len() {
            let scanline_start = y * width as usize * 3;
            // Check for new-style RLE: first 4 bytes = [2, 2, width_hi, width_lo]
            if pos + 4 <= data.len() && data[pos] == 2 && data[pos+1] == 2 {
                let scan_w = ((data[pos+2] as u32) << 8) | data[pos+3] as u32;
                if scan_w != width { return None; }
                pos += 4;
                // RLE decode each of R,G,B,E channels separately
                let mut scanline = vec![0u8; width as usize * 4];
                for ch in 0..4 {
                    let mut x = 0usize;
                    while x < width as usize {
                        if pos >= data.len() { return None; }
                        let code = data[pos]; pos += 1;
                        if code > 128 {
                            // Run
                            let count = (code - 128) as usize;
                            if pos >= data.len() { return None; }
                            let val = data[pos]; pos += 1;
                            for _ in 0..count {
                                if x < width as usize { scanline[x * 4 + ch] = val; }
                                x += 1;
                            }
                        } else {
                            // Literal
                            let count = code as usize;
                            for _ in 0..count {
                                if pos >= data.len() { return None; }
                                if x < width as usize { scanline[x * 4 + ch] = data[pos]; }
                                pos += 1; x += 1;
                            }
                        }
                    }
                }
                // Convert RGBE to float RGB
                for x in 0..width as usize {
                    let (r, g, b, e) = (scanline[x*4], scanline[x*4+1], scanline[x*4+2], scanline[x*4+3]);
                    let idx = scanline_start + x * 3;
                    if e == 0 {
                        pixels[idx] = 0.0; pixels[idx+1] = 0.0; pixels[idx+2] = 0.0;
                    } else {
                        let f = f32::from_bits(((e as u32 + 127 - 128) & 0xFF) << 23); // ldexp(1, e-128)
                        pixels[idx]   = (r as f32 + 0.5) / 256.0 * f;
                        pixels[idx+1] = (g as f32 + 0.5) / 256.0 * f;
                        pixels[idx+2] = (b as f32 + 0.5) / 256.0 * f;
                    }
                }
            } else {
                // Uncompressed RGBE (4 bytes per pixel)
                for x in 0..width as usize {
                    if pos + 3 >= data.len() { break; }
                    let (r, g, b, e) = (data[pos], data[pos+1], data[pos+2], data[pos+3]);
                    pos += 4;
                    let idx = scanline_start + x * 3;
                    if e == 0 {
                        pixels[idx] = 0.0; pixels[idx+1] = 0.0; pixels[idx+2] = 0.0;
                    } else {
                        let f = f32::from_bits(((e as u32 + 127 - 128) & 0xFF) << 23);
                        pixels[idx]   = (r as f32 + 0.5) / 256.0 * f;
                        pixels[idx+1] = (g as f32 + 0.5) / 256.0 * f;
                        pixels[idx+2] = (b as f32 + 0.5) / 256.0 * f;
                    }
                }
            }
            y += 1;
        }
        Some((pixels, width, height))
    }

    // (load_hdr_file / upload_env_to_gpu / generate_env_mips / env_gaussian_blur DELETED
    //  2026-07-18 — replaced by the split-sum IBL pipeline in env_ibl.rs: worker-thread
    //  decode + box placeholder mips + SH9, poll-phase buffer install (Frame Ordering),
    //  GPU GGX prefilter of mips 1..6. The HDRI file dialog now goes through
    //  file_dialog.rs (`request_env_file_dialog`) — never modal inside the egui frame.)

    // ================================================================
    // Per-Primitive Texture Atlas
    // ================================================================

    /// Load an image file into the texture atlas. Returns 1-based texture_id.
    /// Images are capped at 512x512 to conserve GPU memory.
    pub(crate) fn atlas_add_texture(&mut self, path: &std::path::Path) -> Option<u32> {
        let img = image::open(path).ok()?;
        let img = img.resize(512, 512, image::imageops::FilterType::Lanczos3);
        let rgba = img.to_rgba8();
        let (w, h) = rgba.dimensions();
        let offset = self.tex_atlas_data.len() as u32;
        for pixel in rgba.pixels() {
            let packed = (pixel[0] as u32) << 24
                       | (pixel[1] as u32) << 16
                       | (pixel[2] as u32) << 8
                       | pixel[3] as u32;
            self.tex_atlas_data.push(packed);
        }
        let id = self.tex_atlas_next_id;
        self.tex_atlas_next_id += 1;
        self.tex_atlas_meta.push([offset, w, h, 0]);
        self.tex_atlas_dirty = true;
        Some(id)
    }

    /// Upload the texture atlas buffers to GPU. Call when tex_atlas_dirty is true.
    pub(crate) fn upload_tex_atlas_to_gpu(&mut self) {
        if let Some(ref mut gpu) = self.gpu {
            let device = gpu.device.clone();
            let queue = gpu.queue.clone();
            {
                // Atlas pixel data
                let atlas_bytes: Vec<u8> = if self.tex_atlas_data.is_empty() {
                    vec![0u8; 4]
                } else {
                    self.tex_atlas_data.iter().flat_map(|v| v.to_le_bytes()).collect()
                };
                gpu.buffers.tex_atlas_buffer = device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("TexAtlas"), size: atlas_bytes.len() as u64,
                    usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
                queue.write_buffer(&gpu.buffers.tex_atlas_buffer, 0, &atlas_bytes);

                // Atlas metadata (offset, width, height, 0) per texture
                let meta_bytes: Vec<u8> = if self.tex_atlas_meta.is_empty() {
                    vec![0u8; 16]
                } else {
                    self.tex_atlas_meta.iter().flat_map(|m| {
                        m.iter().flat_map(|v| v.to_le_bytes()).collect::<Vec<u8>>()
                    }).collect()
                };
                gpu.buffers.tex_meta_buffer = device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("TexMeta"), size: meta_bytes.len() as u64,
                    usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
                queue.write_buffer(&gpu.buffers.tex_meta_buffer, 0, &meta_bytes);

                // Invalidate all bind groups that reference tex_atlas/tex_meta buffers
                gpu.bind_groups.bind_group = None; gpu.bind_groups.deferred_bg = None; gpu.bind_groups.pt_bg = None;                self.tex_atlas_dirty = false;
            }
        }
    }

    pub(crate) fn request_single_map(host: &str, port: u16, prompt: &str, map_type: &str, size: u32, seed: u32, steps: u32, guidance: f32) -> Result<Vec<u8>, String> {
        Self::request_single_map_masked(host, port, prompt, map_type, size, seed, steps, guidance, None)
    }

    /// Regenerate one map. When `base_png_b64` is set, server soft-composites with a full-white mask
    /// (replace whole map) — same path as material-mask regen with a painted mask later.
    pub(crate) fn request_single_map_masked(
        host: &str,
        port: u16,
        prompt: &str,
        map_type: &str,
        size: u32,
        seed: u32,
        steps: u32,
        guidance: f32,
        base_png_b64: Option<&str>,
    ) -> Result<Vec<u8>, String> {
        use std::io::{Read, Write};
        use std::net::TcpStream;
        let addr = format!("{}:{}", host, port);
        let sa: std::net::SocketAddr = addr.parse().map_err(|e| format!("{}", e))?;
        let mut s = TcpStream::connect_timeout(&sa, std::time::Duration::from_secs(5))
            .map_err(|e| format!("Connect: {}", e))?;
        s.set_read_timeout(Some(std::time::Duration::from_secs(180))).ok();
        // Full-white 1×1 PNG as mask when base is provided → replace entire region with material prompt.
        // Real painted masks can be passed later via the same mask_b64 field.
        let white_mask = if base_png_b64.is_some() {
            // 1x1 white PNG
            Some(Self::encode_base64(&[
                0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52,
                0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x00, 0x00, 0x00, 0x00, 0x3A, 0x7E, 0x9B,
                0x55, 0x00, 0x00, 0x00, 0x0A, 0x49, 0x44, 0x41, 0x54, 0x08, 0xD7, 0x63, 0xF8, 0x0F, 0x00, 0x01,
                0x01, 0x01, 0x00, 0x18, 0xDD, 0x8D, 0xB4, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE,
                0x42, 0x60, 0x82,
            ]))
        } else {
            None
        };
        let body = if let (Some(base), Some(mask)) = (base_png_b64, white_mask.as_deref()) {
            format!(
                r#"{{"prompt":"{}","map_type":"{}","width":{},"height":{},"seed":{},"steps":{},"guidance_scale":{},"base_png_b64":"{}","mask_b64":"{}"}}"#,
                Self::json_escape(prompt), map_type, size, size, seed, steps, guidance, base, mask
            )
        } else {
            format!(
                r#"{{"prompt":"{}","map_type":"{}","width":{},"height":{},"seed":{},"steps":{},"guidance_scale":{}}}"#,
                Self::json_escape(prompt), map_type, size, size, seed, steps, guidance
            )
        };
        let req = format!("POST /generate_map HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", addr, body.len(), body);
        s.write_all(req.as_bytes()).map_err(|e| format!("{}", e))?;
        let mut resp = Vec::new();
        s.read_to_end(&mut resp).map_err(|e| format!("{}", e))?;
        let text = String::from_utf8_lossy(&resp).to_string();
        let bs = text.find("\r\n\r\n").ok_or("No headers")?;
        let hb = if text.contains("chunked") { Self::decode_chunked(&text[bs+4..])? } else { text[bs+4..].to_string() };
        let key = format!("\"{}_png_b64\":\"", map_type);
        let st = hb.find(&key).ok_or(format!("No {} in response", map_type))?;
        let ds = st + key.len();
        let end = hb[ds..].find('"').ok_or("Unterminated")?;
        Self::decode_base64(&hb[ds..ds+end])
    }

    /// Fill object prompt slots from the current scene selection (meshes / bodies / materials).
    pub(crate) fn ai_tex_pull_slots_from_selection(&mut self) {
        use crate::AiObjectPromptSlot;
        let mut slots = Vec::new();
        let mut idx = 1usize;
        // Collect (nid, display name) first — avoid borrow conflicts with slots.
        let mut picked: Vec<(core_math::scene_graph::NodeId, String)> = Vec::new();
        for item in self.sel.usel.selected.iter() {
            if let crate::SceneItemId::Node(nid) = item {
                if let Some(node) = self.scene.scene_graph.get(*nid) {
                    let name = node.payload.display_name().to_string();
                    let name = if name.is_empty() {
                        format!("Mesh {:02}", idx)
                    } else {
                        name
                    };
                    picked.push((*nid, name));
                    idx += 1;
                }
            }
        }
        if picked.is_empty() {
            if let Some(nid) = self.sel.usel.active_node() {
                if let Some(node) = self.scene.scene_graph.get(nid) {
                    let name = node.payload.display_name().to_string();
                    let name = if name.is_empty() { "Active mesh".into() } else { name };
                    picked.push((nid, name));
                }
            }
        }
        for (i, (nid, name)) in picked.into_iter().enumerate() {
            let id = format!("mesh-{:02}", i + 1);
            let existing = self.ai.ai_tex_object_slots.iter()
                .find(|s| s.node_id == Some(nid))
                .map(|s| s.prompt.clone())
                .unwrap_or_default();
            slots.push(AiObjectPromptSlot {
                id,
                name,
                prompt: existing,
                node_id: Some(nid),
            });
        }
        if slots.is_empty() {
            self.toast("Select meshes/bodies first, then From selection");
            return;
        }
        self.ai.ai_tex_object_slots = slots;
        self.ai.ai_tex_multi_object = true;
        self.toast(&format!(
            "{} object slots ready — fill material prompts (leather / steel / \u{2026})",
            self.ai.ai_tex_object_slots.len()
        ));
    }
}
