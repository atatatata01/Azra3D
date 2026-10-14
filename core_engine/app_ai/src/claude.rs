// core_engine/app/src/claude.rs
// ====================================================================
// AZRA — Claude API Bridge
// ====================================================================
// Provides two capabilities:
//   1. Code Review   — ask Claude about the Rust source code
//   2. 3D Generation — Claude as a fallback for Ollama (same JSON format)
//
// Architecture: identical to agent.rs
//   UI Thread  ──send(request)──▶  Worker Thread (blocking HTTPS via ureq)
//   UI Thread  ◀──recv(result)──   Worker Thread
// ====================================================================

use std::sync::mpsc;
use std::thread;
use std::time::Instant;

// ====================================================================
// Config
// ====================================================================
#[derive(Clone)]
pub struct ClaudeConfig {
    pub api_key: String,
    pub model: String,
}

impl Default for ClaudeConfig {
    fn default() -> Self {
        Self {
            api_key: std::env::var("ANTHROPIC_API_KEY").unwrap_or_default(),
            model: "claude-sonnet-4-6".into(),
        }
    }
}

// ====================================================================
// Request / Response
// ====================================================================
#[derive(Clone, PartialEq)]
pub enum ClaudeKind { CodeReview, Generate3D, EnhancePrompt, DescribeImage, GenerateNodeGraph }

pub struct ClaudeRequest {
    pub kind:      ClaudeKind,
    pub prompt:    String,   // user's question or description
    pub context:   String,   // source code or scene JSON (may be empty)
    pub image_b64: String,   // base64 image data for DescribeImage (empty otherwise)
}

pub struct ClaudeResponse {
    pub text: String,
    pub kind: ClaudeKind,
}

// ====================================================================
// Bridge (owned by AzraApp)
// ====================================================================
pub struct ClaudeBridge {
    tx: mpsc::Sender<(ClaudeRequest, ClaudeConfig)>,
    rx: mpsc::Receiver<Result<ClaudeResponse, String>>,
    pub is_thinking: bool,
    pub thinking_start: Instant,
}

impl ClaudeBridge {
    pub fn new() -> Self {
        let (tx, rx_in)  = mpsc::channel::<(ClaudeRequest, ClaudeConfig)>();
        let (tx_out, rx) = mpsc::channel::<Result<ClaudeResponse, String>>();

        thread::spawn(move || {
            while let Ok((req, cfg)) = rx_in.recv() {
                let res = call_claude(&req, &cfg);
                if tx_out.send(res).is_err() { break; }
            }
        });

        Self { tx, rx, is_thinking: false, thinking_start: Instant::now() }
    }

    pub fn send(&mut self, req: ClaudeRequest, cfg: &ClaudeConfig) {
        self.is_thinking = true;
        self.thinking_start = Instant::now();
        let _ = self.tx.send((req, cfg.clone()));
    }

    pub fn poll(&mut self) -> Option<Result<ClaudeResponse, String>> {
        match self.rx.try_recv() {
            Ok(r)  => { self.is_thinking = false; Some(r) }
            Err(mpsc::TryRecvError::Empty) => None,
            Err(_) => { self.is_thinking = false; Some(Err("Claude thread crashed".into())) }
        }
    }

    pub fn thinking_seconds(&self) -> f32 {
        self.thinking_start.elapsed().as_secs_f32()
    }

    pub fn key_ok(cfg: &ClaudeConfig) -> bool {
        cfg.api_key.starts_with("sk-")
    }
}

// ====================================================================
// HTTPS call via ureq (blocking — runs on worker thread)
// ====================================================================
fn call_claude(req: &ClaudeRequest, cfg: &ClaudeConfig) -> Result<ClaudeResponse, String> {
    if cfg.api_key.is_empty() {
        return Err(
            "ANTHROPIC_API_KEY not set.\n\
             Add this line to Run_Azra.bat:\n\
             set ANTHROPIC_API_KEY=sk-ant-...".into()
        );
    }

    // Build JSON body manually (no serde)
    let body = if req.kind == ClaudeKind::DescribeImage {
        // Vision API — content is an array with image + text blocks
        let media = detect_media_type(&req.image_b64);
        let text  = "Describe the material or surface texture in this image in 1-2 sentences. \
                     Focus on: colour, finish (matte/glossy/rough), material type, wear or aging. \
                     Be specific and technical. Output only the description, no preamble.";
        format!(
            r#"{{"model":"{}","max_tokens":256,"messages":[{{"role":"user","content":[{{"type":"image","source":{{"type":"base64","media_type":"{}","data":"{}"}}}},{{"type":"text","text":"{}"}}]}}]}}"#,
            escape_json(&cfg.model), media, req.image_b64, escape_json(text)
        )
    } else {
        let (system, user) = build_messages(req);
        format!(
            r#"{{"model":"{}","max_tokens":4096,"system":"{}","messages":[{{"role":"user","content":"{}"}}]}}"#,
            escape_json(&cfg.model),
            escape_json(&system),
            escape_json(&user),
        )
    };

    let resp = ureq::post("https://api.anthropic.com/v1/messages")
        .set("x-api-key",          &cfg.api_key)
        .set("anthropic-version",  "2023-06-01")
        .set("content-type",       "application/json")
        .timeout(std::time::Duration::from_secs(120))
        .send_string(&body)
        .map_err(|e| match e {
            ureq::Error::Status(code, resp) => {
                let b = resp.into_string().unwrap_or_default();
                format!("API error {}: {}", code, &b[..b.len().min(300)])
            }
            ureq::Error::Transport(t) => format!("Network error: {}", t),
        })?;

    let body = resp.into_string()
        .map_err(|e| format!("Response read failed: {}", e))?;

    let text = extract_content_text(&body)
        .ok_or_else(|| format!("Could not parse response: {}", &body[..body.len().min(300)]))?;

    Ok(ClaudeResponse { text, kind: req.kind.clone() })
}

fn detect_media_type(b64: &str) -> &'static str {
    if      b64.starts_with("/9j/")  { "image/jpeg" }
    else if b64.starts_with("iVBOR") { "image/png"  }
    else if b64.starts_with("R0lGO") { "image/gif"  }
    else if b64.starts_with("UklGR") { "image/webp" }
    else                             { "image/jpeg" }  // safe default
}

fn build_messages(req: &ClaudeRequest) -> (String, String) {
    match req.kind {
        ClaudeKind::CodeReview => {
            let system =
                "You are a senior Rust/graphics engineer reviewing AZRA, \
                 a Rust/wgpu/egui SDF 3D modeler. Give concise, actionable feedback. \
                 Use markdown with code blocks. Focus on correctness, performance, \
                 and egui patterns. Be specific about line numbers when possible.".to_string();
            let user = if req.context.is_empty() {
                req.prompt.clone()
            } else {
                format!("## Source code\n```rust\n{}\n```\n\n## Question\n{}", req.context, req.prompt)
            };
            (system, user)
        }
        ClaudeKind::Generate3D => {
            let system =
                "You are AZRA 3D Agent. The user describes a 3D object or scene. \
                 Decompose it into simple SDF primitives. \
                 RESPOND WITH ONLY A JSON ARRAY. No explanation, no markdown, no backticks. \
                 Each element: {\"shape\",\"name\",\"pos\":[x,y,z],\"rot\":[rx,ry,rz],\"op\":\"Union\",\"k\":0.1}. \
                 Shapes: Sphere {\"params\":[r]}, Box {\"scale\":[hx,hy,hz]}, \
                 Cylinder/Capsule/Cone {\"params\":[r,h]}, Torus {\"params\":[major,minor]}. \
                 Coordinates -2.0 to 2.0. Y is up.".to_string();
            let user = if req.context.is_empty() {
                format!("Generate: {}", req.prompt)
            } else {
                format!("Current scene:\n{}\n\nRequest: {}", req.context, req.prompt)
            };
            (system, user)
        }
        ClaudeKind::EnhancePrompt => {
            let system =
                "You are a Stable Diffusion prompt engineer. \
                 Rewrite the user's short material/texture description into a rich, detailed \
                 SD prompt (max 120 words). Include: surface texture, micro-detail, lighting cues, \
                 material properties. Output ONLY the enhanced prompt — no explanation, no quotes.".to_string();
            (system, req.prompt.clone())
        }
        ClaudeKind::GenerateNodeGraph => {
            let system =
                "You are a procedural material expert for a node-based shader editor. \
                 The user describes a material. Generate a NodeGraph as valid JSON. \
                 RESPOND WITH ONLY THE JSON OBJECT — no explanation, no markdown, no backticks.\n\n\
                 The JSON must match this Rust struct:\n\
                 {\"nodes\":[...],\"connections\":[...],\"next_id\":N,\"wgsl_cache\":null}\n\n\
                 Each node: {\"id\":N,\"node_type\":\"...\",\"position\":[x,y],\"inputs\":[{\"name\":\"...\",\"socket_type\":\"Float\",\"default_value\":0.0},...],\"outputs\":[{\"name\":\"...\",\"socket_type\":\"Float\"},...],\"label\":\"...\",\"math_op\":null,\"math_unary_op\":null,\"color_ramp_stops\":null}\n\n\
                 Available node_type values: \"TexCoord\", \"ObjectInfo\", \"Noise\", \"Voronoi\", \
                 \"MathBinary\", \"MathUnary\", \"Remap\", \"ColorRamp\", \"MixRgb\", \
                 \"BrightnessContrast\", \"SeparateXyz\", \"CombineXyz\", \
                 \"WaveTexture\", \"MusgraveTexture\", \"BrickTexture\", \"GradientTexture\", \
                 \"MaterialOutput\"\n\n\
                 Socket types: \"Float\", \"Vec3\", \"Color\"\n\
                 MathBinary ops: \"Add\",\"Subtract\",\"Multiply\",\"Divide\",\"Power\",\"Min\",\"Max\",\"Modulo\"\n\
                 MathUnary ops: \"Abs\",\"Floor\",\"Ceil\",\"Fract\",\"Sqrt\",\"Sin\",\"Cos\",\"Negate\",\"Smoothstep\"\n\n\
                 Each connection: {\"from_node\":N,\"from_output\":M,\"to_node\":N,\"to_input\":M}\n\n\
                 ALWAYS include a MaterialOutput node (with inputs: base_color Vec3, roughness Float, metallic Float, emission Color, normal Vec3).\n\
                 ALWAYS include a TexCoord node as the coordinate source.\n\
                 Position nodes left-to-right: inputs at x=50, generators at x=200, math/color at x=400, output at x=600.\n\
                 Create realistic, high-quality procedural materials. Use noise/voronoi for organic surfaces, \
                 wave/brick for regular patterns, color ramps for artistic control.".to_string();
            let user = format!("Generate a procedural material node graph for: {}", req.prompt);
            (system, user)
        }
        ClaudeKind::DescribeImage => unreachable!("DescribeImage handled before build_messages"),
    }
}

// ====================================================================
// Response parser — extracts content[0].text from Claude JSON
// ====================================================================
fn extract_content_text(json: &str) -> Option<String> {
    // Response format: {"content":[{"type":"text","text":"..."}],...}
    let key = "\"text\":\"";
    let start = json.find(key)? + key.len();
    let rest  = &json[start..];
    let mut result = String::new();
    let mut chars  = rest.chars();
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
            Some('"') => break,
            Some(c)   => result.push(c),
            None      => break,
        }
    }
    if result.is_empty() { None } else { Some(result) }
}

// ====================================================================
// JSON string escaper (same as agent.rs)
// ====================================================================
/// Test-visible wrapper for escape_json.
pub fn escape_json_test(s: &str) -> String { escape_json(s) }

fn escape_json(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 16);
    for ch in s.chars() {
        match ch {
            '"'  => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {}
            c    => out.push(c),
        }
    }
    out
}
