// core_engine/app/src/ai_client.rs
// ====================================================================
// AZRA — AI Service Client
// ====================================================================
// Async HTTP client for the Python AI service (ai_service.py on port 7861).
// Provides material analysis (CLIP) and text-to-profile endpoints.
//
// Architecture: same channel pattern as agent.rs / claude.rs
//   UI Thread  ──send(request)──▶  Worker Thread (blocking HTTP via ureq)
//   UI Thread  ◀──recv(result)──   Worker Thread
// ====================================================================

use std::sync::mpsc;
use std::thread;
use std::time::Duration;

// ====================================================================
// Config
// ====================================================================
const DEFAULT_URL: &str = "http://127.0.0.1:7861";

// ====================================================================
// Request / Response
// ====================================================================
#[derive(Clone)]
#[allow(dead_code)]
pub enum AiRequest {
    /// Analyze an image (PNG/JPG bytes) → material classification + PBR
    AnalyzeImage { image_bytes: Vec<u8> },
    /// Convert text description → PBR material properties
    TextToProfile { description: String },
    /// Check if the service is running
    HealthCheck,
}

#[derive(Clone, Debug)]
pub struct PbrProfile {
    pub roughness: f32,
    pub metallic: f32,
    pub albedo: [f32; 3],
    pub matched: Option<String>,
}

#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct ImageAnalysis {
    pub top_material: String,
    pub confidence: f32,
    pub pbr: PbrProfile,
    pub elapsed_ms: f32,
}

#[derive(Clone, Debug)]
pub enum AiResponse {
    ImageResult(ImageAnalysis),
    ProfileResult(PbrProfile),
    Health { ok: bool, clip_loaded: bool },
    Error(String),
}

// ====================================================================
// Bridge (channel-based async)
// ====================================================================
pub struct AiBridge {
    tx: mpsc::Sender<AiRequest>,
    rx: mpsc::Receiver<AiResponse>,
    pub busy: bool,
}

impl AiBridge {
    pub fn new() -> Self {
        let (req_tx, req_rx) = mpsc::channel::<AiRequest>();
        let (res_tx, res_rx) = mpsc::channel::<AiResponse>();

        thread::Builder::new()
            .name("ai-client".into())
            .spawn(move || worker(req_rx, res_tx))
            .expect("spawn ai-client thread");

        Self { tx: req_tx, rx: res_rx, busy: false }
    }

    pub fn send(&mut self, req: AiRequest) {
        if self.tx.send(req).is_ok() {
            self.busy = true;
        }
    }

    pub fn poll(&mut self) -> Option<AiResponse> {
        match self.rx.try_recv() {
            Ok(resp) => { self.busy = false; Some(resp) }
            Err(_) => None,
        }
    }
}

// ====================================================================
// Worker thread
// ====================================================================
fn worker(rx: mpsc::Receiver<AiRequest>, tx: mpsc::Sender<AiResponse>) {
    let base_url = std::env::var("AZRA_AI_URL").unwrap_or_else(|_| DEFAULT_URL.into());
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(60))
        .build();

    while let Ok(req) = rx.recv() {
        let resp = match req {
            AiRequest::HealthCheck => {
                match agent.get(&format!("{}/health", base_url)).call() {
                    Ok(r) => {
                        if let Ok(body) = r.into_string() {
                            let ok = body.contains("\"ok\"");
                            let clip = body.contains("\"clip_loaded\":true")
                                || body.contains("\"clip_loaded\": true");
                            AiResponse::Health { ok, clip_loaded: clip }
                        } else {
                            AiResponse::Health { ok: true, clip_loaded: false }
                        }
                    }
                    Err(e) => AiResponse::Error(format!("AI service unavailable: {e}")),
                }
            }

            AiRequest::AnalyzeImage { image_bytes } => {
                let b64 = base64_encode(&image_bytes);
                match agent.post(&format!("{}/analyze_image", base_url))
                    .send_form(&[("image_b64", &b64)])
                {
                    Ok(r) => parse_image_result(r),
                    Err(e) => AiResponse::Error(format!("analyze_image failed: {e}")),
                }
            }

            AiRequest::TextToProfile { description } => {
                match agent.post(&format!("{}/text_to_profile", base_url))
                    .send_form(&[("description", &description)])
                {
                    Ok(r) => parse_profile_result(r),
                    Err(e) => AiResponse::Error(format!("text_to_profile failed: {e}")),
                }
            }
        };

        if tx.send(resp).is_err() { break; }
    }
}

// ====================================================================
// Parsing helpers
// ====================================================================
fn parse_image_result(resp: ureq::Response) -> AiResponse {
    let body = match resp.into_string() {
        Ok(s) => s,
        Err(e) => return AiResponse::Error(format!("read body: {e}")),
    };

    // Minimal JSON parsing without serde
    let top = json_str(&body, "top_material").unwrap_or_default();
    let confidence = json_f32(&body, "confidence").unwrap_or(0.0);
    let elapsed = json_f32(&body, "elapsed_ms").unwrap_or(0.0);

    // Parse nested pbr object
    let pbr = if let Some(pbr_start) = body.find("\"pbr\"") {
        let sub = &body[pbr_start..];
        let roughness = json_f32(sub, "roughness").unwrap_or(0.5);
        let metallic = json_f32(sub, "metallic").unwrap_or(0.0);
        let albedo = parse_albedo(sub);
        PbrProfile { roughness, metallic, albedo, matched: Some(top.clone()) }
    } else {
        PbrProfile { roughness: 0.5, metallic: 0.0, albedo: [0.5, 0.5, 0.5], matched: None }
    };

    AiResponse::ImageResult(ImageAnalysis { top_material: top, confidence, pbr, elapsed_ms: elapsed })
}

fn parse_profile_result(resp: ureq::Response) -> AiResponse {
    let body = match resp.into_string() {
        Ok(s) => s,
        Err(e) => return AiResponse::Error(format!("read body: {e}")),
    };

    let roughness = json_f32(&body, "roughness").unwrap_or(0.5);
    let metallic = json_f32(&body, "metallic").unwrap_or(0.0);
    let matched = json_str(&body, "matched");
    let albedo = parse_albedo(&body);

    AiResponse::ProfileResult(PbrProfile { roughness, metallic, albedo, matched })
}

fn parse_albedo(s: &str) -> [f32; 3] {
    if let Some(start) = s.find("\"albedo\"") {
        let sub = &s[start..];
        if let Some(bracket) = sub.find('[') {
            if let Some(end) = sub[bracket..].find(']') {
                let nums: Vec<f32> = sub[bracket+1..bracket+end]
                    .split(',')
                    .filter_map(|v| v.trim().parse().ok())
                    .collect();
                if nums.len() >= 3 {
                    return [nums[0], nums[1], nums[2]];
                }
            }
        }
    }
    [0.5, 0.5, 0.5]
}

/// Extract a string value from JSON (simple, no serde)
fn json_str(json: &str, key: &str) -> Option<String> {
    let pattern = format!("\"{}\"", key);
    let start = json.find(&pattern)?;
    let rest = &json[start + pattern.len()..];
    // Skip : and whitespace
    let rest = rest.trim_start().strip_prefix(':')?;
    let rest = rest.trim_start();
    if rest.starts_with('"') {
        let inner = &rest[1..];
        let end = inner.find('"')?;
        Some(inner[..end].to_string())
    } else if rest.starts_with("null") {
        None
    } else {
        None
    }
}

/// Extract a float value from JSON (simple, no serde)
fn json_f32(json: &str, key: &str) -> Option<f32> {
    let pattern = format!("\"{}\"", key);
    let start = json.find(&pattern)?;
    let rest = &json[start + pattern.len()..];
    let rest = rest.trim_start().strip_prefix(':')?;
    let rest = rest.trim_start();
    // Read until non-numeric (including scientific notation: e.g. 1.5e-3)
    let end = rest.find(|c: char| !c.is_ascii_digit() && c != '.' && c != '-' && c != 'e' && c != 'E' && c != '+').unwrap_or(rest.len());
    rest[..end].parse().ok()
}

/// Base64 encode bytes (no padding, standard alphabet)
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
