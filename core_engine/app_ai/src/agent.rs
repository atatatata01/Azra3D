// core_engine/app/src/agent.rs
// ====================================================================
// AZRA — Agent Bridge (Text-to-CSG via local LLM)
// ====================================================================
// Architecture:
//   UI Thread  ──send(prompt)──▶  Worker Thread (LLM inference)
//   UI Thread  ◀──recv(result)──  Worker Thread
//   The UI never blocks. Worker runs in background.
//
//   Supports: Ollama (localhost:11434) or Mock fallback
// ====================================================================

use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};
use std::io::{Read, Write};
use std::net::TcpStream;
use glam::Vec3;
use core_math::primitives::{SdfPrimitive, SdfShape, BooleanOp, BoxBevel};

// ====================================================================
// System Prompt — The "brain" of the agent
// ====================================================================
const SYSTEM_PROMPT: &str = r#"You are AZRA Agent, a 3D blockout assistant. The user describes an object. You decompose it into simple SDF primitives.

Available shapes: Sphere, Box, Cylinder, Capsule, Cone, Torus
Workspace: coordinates from -2.0 to 2.0. Keep objects within this range.
Coordinate system: Y is up. Center objects around Y=0 (ground level).

Boolean operations (field "op"): Union (default, smooth if k>0), Subtract, Intersect. Add "k" field (0.0–0.5) for blend radius; k=0 means hard edge.

RESPOND WITH ONLY A JSON ARRAY. No explanation, no markdown, no backticks. Just the raw JSON array.

Each element has:
- "shape": one of the shape names above
- "name": descriptive part name (e.g. "Barrel", "Grip", "Hull")
- "pos": [x, y, z] position
- "rot": [rx, ry, rz] rotation in degrees (optional, default [0,0,0])
- "op": boolean operation (optional, default "Union")
- "k": blend radius 0.0–0.5 (optional, default 0.15; 0.0 = hard edge, ignored for Intersect)

For Sphere: "params": [radius]
For Box: "scale": [half_x, half_y, half_z]
For Cylinder: "params": [radius, half_height]
For Capsule: "params": [radius, half_height]
For Cone: "params": [radius, half_height]
For Torus: "params": [major_radius, minor_radius]

Example for a simple table:
[{"shape":"Box","name":"Top","scale":[0.8,0.03,0.4],"pos":[0,0.4,0]},{"shape":"Cylinder","name":"Leg1","params":[0.03,0.38],"pos":[-0.65,0,0.32]},{"shape":"Cylinder","name":"Leg2","params":[0.03,0.38],"pos":[0.65,0,0.32]},{"shape":"Cylinder","name":"Leg3","params":[0.03,0.38],"pos":[-0.65,0,-0.32]},{"shape":"Cylinder","name":"Leg4","params":[0.03,0.38],"pos":[0.65,0,-0.32]}]

Think step-by-step about the object's anatomy, then output ONLY the JSON array."#;

// ====================================================================
// Agent Configuration
// ====================================================================
#[derive(Clone)]
pub struct AgentConfig {
    pub use_ollama: bool,
    pub ollama_host: String,
    pub ollama_port: u16,
    pub model_name: String,
    pub temperature: f32,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            use_ollama: true,
            ollama_host: "127.0.0.1".into(),
            ollama_port: 11434,
            model_name: "gpt-oss:20b".into(),
            temperature: 0.3,
        }
    }
}

// ====================================================================
// Agent Status (visible to UI)
// ====================================================================
#[derive(Clone)]
#[allow(dead_code)]
pub enum AgentStatus {
    Idle,
    Thinking { prompt: String, started: Instant },
    Error(String),
}

// ====================================================================
// Agent Bridge (owned by AzraApp)
// ====================================================================
pub struct AgentBridge {
    tx: mpsc::Sender<(String, AgentConfig)>,
    rx: mpsc::Receiver<AgentResult>,
    pub status: AgentStatus,
    pub last_raw_json: String,
    pub ollama_available: Option<bool>,
}

enum AgentResult {
    Success { prims: Vec<SdfPrimitive>, raw_json: String, source: String },
    Error(String),
}

impl AgentBridge {
    pub fn new() -> Self {
        let (tx_prompt, rx_prompt) = mpsc::channel::<(String, AgentConfig)>();
        let (tx_result, rx_result) = mpsc::channel::<AgentResult>();

        thread::spawn(move || {
            while let Ok((prompt, config)) = rx_prompt.recv() {
                let result = generate_and_parse(&prompt, &config);
                let _ = tx_result.send(result);
            }
        });

        Self {
            tx: tx_prompt,
            rx: rx_result,
            status: AgentStatus::Idle,
            last_raw_json: String::new(),
            ollama_available: None,
        }
    }

    pub fn send_prompt(&mut self, prompt: String, config: &AgentConfig) {
        self.status = AgentStatus::Thinking {
            prompt: prompt.clone(),
            started: Instant::now(),
        };
        let _ = self.tx.send((prompt, config.clone()));
    }

    pub fn poll(&mut self) -> Option<Result<Vec<SdfPrimitive>, String>> {
        match self.rx.try_recv() {
            Ok(AgentResult::Success { prims, raw_json, source }) => {
                self.last_raw_json = raw_json;
                self.status = AgentStatus::Idle;
                if source == "ollama" { self.ollama_available = Some(true); }
                Some(Ok(prims))
            }
            Ok(AgentResult::Error(e)) => {
                self.status = AgentStatus::Error(e.clone());
                Some(Err(e))
            }
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => {
                self.status = AgentStatus::Error("Agent thread crashed".into());
                Some(Err("Agent disconnected".into()))
            }
        }
    }

    pub fn is_thinking(&self) -> bool {
        matches!(self.status, AgentStatus::Thinking { .. })
    }

    pub fn thinking_seconds(&self) -> f32 {
        if let AgentStatus::Thinking { started, .. } = &self.status {
            started.elapsed().as_secs_f32()
        } else { 0.0 }
    }

    /// Check if Ollama is reachable (blocking, ~2s timeout)
    pub fn check_ollama(config: &AgentConfig) -> bool {
        let addr = format!("{}:{}", config.ollama_host, config.ollama_port);
        if let Ok(sock_addr) = addr.parse::<std::net::SocketAddr>() {
            if let Ok(mut stream) = TcpStream::connect_timeout(&sock_addr, Duration::from_secs(2)) {
                let req = format!("GET / HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n", addr);
                if stream.write_all(req.as_bytes()).is_ok() {
                    let mut buf = [0u8; 256];
                    if let Ok(n) = stream.read(&mut buf) {
                        let resp = String::from_utf8_lossy(&buf[..n]);
                        return resp.contains("200") || resp.contains("Ollama");
                    }
                }
            }
        }
        false
    }
}

// ====================================================================
// Generation Pipeline
// ====================================================================
fn generate_and_parse(prompt: &str, config: &AgentConfig) -> AgentResult {
    let (json, source) = if config.use_ollama {
        match call_ollama(prompt, config) {
            Ok(response) => {
                match extract_json_array(&response) {
                    Some(json) => (json, "ollama".to_string()),
                    None => {
                        return AgentResult::Error(format!(
                            "Ollama responded but no JSON array found.\nRaw: {}",
                            &response[..response.len().min(400)]
                        ));
                    }
                }
            }
            Err(e) => {
                eprintln!("[Agent] Ollama unavailable ({}), falling back to mock", e);
                (mock_generate(prompt), "mock (Ollama offline)".to_string())
            }
        }
    } else {
        thread::sleep(Duration::from_secs(2));
        (mock_generate(prompt), "mock".to_string())
    };

    match parse_agent_json(&json) {
        Ok(prims) => AgentResult::Success { prims, raw_json: json, source },
        Err(e) => AgentResult::Error(format!("Parse error: {} | JSON: {}", e, &json[..json.len().min(300)])),
    }
}

// ====================================================================
// Ollama HTTP Client (raw TCP, zero external dependencies)
// ====================================================================
fn call_ollama(prompt: &str, config: &AgentConfig) -> Result<String, String> {
    let addr = format!("{}:{}", config.ollama_host, config.ollama_port);
    let sock_addr: std::net::SocketAddr = addr.parse()
        .map_err(|e| format!("Bad address: {}", e))?;

    let mut stream = TcpStream::connect_timeout(&sock_addr, Duration::from_secs(5))
        .map_err(|e| format!("Connection failed: {}. Is Ollama running? (ollama serve)", e))?;

    stream.set_read_timeout(Some(Duration::from_secs(120)))
        .map_err(|e| format!("Timeout config: {}", e))?;

    // Build JSON body manually (no serde dependency)
    let system_esc = escape_json_string(SYSTEM_PROMPT);
    let prompt_esc = escape_json_string(prompt);
    let model_esc = escape_json_string(&config.model_name);
    let body = format!(
        r#"{{"model":"{}","system":"{}","prompt":"Blockout: {}","stream":false,"options":{{"temperature":{:.1},"num_predict":2048}}}}"#,
        model_esc, system_esc, prompt_esc, config.temperature
    );

    let request = format!(
        "POST /api/generate HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        addr, body.len(), body
    );

    stream.write_all(request.as_bytes()).map_err(|e| format!("Write: {}", e))?;

    let mut response_bytes = Vec::new();
    stream.read_to_end(&mut response_bytes).map_err(|e| format!("Read: {}", e))?;
    let response = String::from_utf8_lossy(&response_bytes).to_string();

    // Split HTTP headers from body
    if let Some(body_start) = response.find("\r\n\r\n") {
        let http_body = &response[body_start + 4..];
        if let Some(resp_text) = extract_ollama_response(http_body) {
            Ok(resp_text)
        } else {
            Err(format!("Could not parse Ollama response: {}", &http_body[..http_body.len().min(400)]))
        }
    } else {
        Err(format!("No HTTP headers: {}", &response[..response.len().min(200)]))
    }
}

/// Extract the "response" field from Ollama JSON, handling escaped chars
fn extract_ollama_response(json: &str) -> Option<String> {
    let key = "\"response\":\"";
    let start = json.find(key)? + key.len();
    let rest = &json[start..];
    let mut result = String::new();
    let mut chars = rest.chars();
    loop {
        match chars.next() {
            Some('\\') => match chars.next() {
                Some('n') => result.push('\n'),
                Some('t') => result.push('\t'),
                Some('r') => {},
                Some('"') => result.push('"'),
                Some('\\') => result.push('\\'),
                Some(c) => { result.push('\\'); result.push(c); }
                None => break,
            },
            Some('"') => break,
            Some(c) => result.push(c),
            None => break,
        }
    }
    if result.is_empty() { None } else { Some(result) }
}

/// Extract a JSON array from text (LLM might wrap it in markdown/explanation)
fn extract_json_array(text: &str) -> Option<String> {
    let start = text.find('[')?;
    let bytes = text.as_bytes();
    let mut depth = 0i32;
    let mut end = start;
    let mut in_string = false;
    let mut i = start;
    while i < bytes.len() {
        let b = bytes[i];
        if in_string {
            if b == b'\\' {
                // Skip the escaped character entirely (handles \", \\, etc.)
                i += 2;
                continue;
            }
            if b == b'"' {
                in_string = false;
            }
        } else {
            match b {
                b'"' => { in_string = true; }
                b'[' => { depth += 1; }
                b']' => { depth -= 1; if depth == 0 { end = i; break; } }
                _ => {}
            }
        }
        i += 1;
    }
    if depth == 0 && end > start { Some(text[start..=end].to_string()) } else { None }
}

fn escape_json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 16);
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {},
            c => out.push(c),
        }
    }
    out
}

// ====================================================================
// Mock Generator (fallback when Ollama not running)
// ====================================================================
fn mock_generate(prompt: &str) -> String {
    let lower = prompt.to_lowercase();
    if lower.contains("ak") || lower.contains("rifle") || lower.contains("gun") || lower.contains("fusil") {
        r#"[{"shape":"Box","name":"Receiver","scale":[0.4,0.1,0.05],"pos":[0,0.1,0]},{"shape":"Cylinder","name":"Barrel","params":[0.025,0.5],"pos":[0.7,0.14,0],"rot":[0,0,90]},{"shape":"Box","name":"Magazine","scale":[0.06,0.14,0.03],"pos":[0.15,-0.08,0],"rot":[0,0,-12]},{"shape":"Capsule","name":"Grip","params":[0.03,0.08],"pos":[-0.15,-0.1,0],"rot":[0,0,15]},{"shape":"Box","name":"Stock","scale":[0.22,0.06,0.035],"pos":[-0.55,0.06,0]},{"shape":"Cylinder","name":"Muzzle","params":[0.03,0.04],"pos":[1.0,0.14,0],"rot":[0,0,90]},{"shape":"Box","name":"HandGuard","scale":[0.18,0.045,0.045],"pos":[0.35,0.1,0]}]"#.into()
    } else if lower.contains("tank") || lower.contains("char") {
        r#"[{"shape":"Box","name":"Hull","scale":[0.7,0.2,0.4],"pos":[0,0,0]},{"shape":"Box","name":"Turret","scale":[0.3,0.15,0.3],"pos":[0.05,0.3,0]},{"shape":"Cylinder","name":"Cannon","params":[0.04,0.6],"pos":[0.7,0.35,0],"rot":[0,0,90]},{"shape":"Cylinder","name":"WheelL1","params":[0.12,0.08],"pos":[-0.4,-0.18,0.45],"rot":[90,0,0]},{"shape":"Cylinder","name":"WheelL2","params":[0.12,0.08],"pos":[0,-0.18,0.45],"rot":[90,0,0]},{"shape":"Cylinder","name":"WheelL3","params":[0.12,0.08],"pos":[0.4,-0.18,0.45],"rot":[90,0,0]},{"shape":"Cylinder","name":"WheelR1","params":[0.12,0.08],"pos":[-0.4,-0.18,-0.45],"rot":[90,0,0]},{"shape":"Cylinder","name":"WheelR2","params":[0.12,0.08],"pos":[0,-0.18,-0.45],"rot":[90,0,0]},{"shape":"Cylinder","name":"WheelR3","params":[0.12,0.08],"pos":[0.4,-0.18,-0.45],"rot":[90,0,0]}]"#.into()
    } else if lower.contains("head") || lower.contains("face") || lower.contains("tête") || lower.contains("orc") {
        r#"[{"shape":"Sphere","name":"Skull","params":[0.45],"pos":[0,0,0]},{"shape":"Sphere","name":"Jaw","params":[0.28],"pos":[0,-0.25,0.15]},{"shape":"Sphere","name":"EyeL","params":[0.08],"pos":[-0.15,0.08,0.35],"op":"Subtract"},{"shape":"Sphere","name":"EyeR","params":[0.08],"pos":[0.15,0.08,0.35],"op":"Subtract"},{"shape":"Box","name":"Nose","scale":[0.06,0.08,0.1],"pos":[0,-0.02,0.42]},{"shape":"Cylinder","name":"Neck","params":[0.15,0.2],"pos":[0,-0.55,0]}]"#.into()
    } else if lower.contains("table") || lower.contains("desk") {
        r#"[{"shape":"Box","name":"Top","scale":[0.8,0.03,0.4],"pos":[0,0.4,0]},{"shape":"Cylinder","name":"LegFL","params":[0.03,0.38],"pos":[-0.65,0,0.32]},{"shape":"Cylinder","name":"LegFR","params":[0.03,0.38],"pos":[0.65,0,0.32]},{"shape":"Cylinder","name":"LegBL","params":[0.03,0.38],"pos":[-0.65,0,-0.32]},{"shape":"Cylinder","name":"LegBR","params":[0.03,0.38],"pos":[0.65,0,-0.32]}]"#.into()
    } else if lower.contains("car") || lower.contains("voiture") || lower.contains("vehicle") {
        r#"[{"shape":"Box","name":"Body","scale":[0.8,0.18,0.35],"pos":[0,0.15,0]},{"shape":"Box","name":"Cabin","scale":[0.35,0.16,0.32],"pos":[-0.05,0.45,0]},{"shape":"Sphere","name":"WheelFL","params":[0.12],"pos":[0.5,-0.05,0.38]},{"shape":"Sphere","name":"WheelFR","params":[0.12],"pos":[0.5,-0.05,-0.38]},{"shape":"Sphere","name":"WheelBL","params":[0.12],"pos":[-0.45,-0.05,0.38]},{"shape":"Sphere","name":"WheelBR","params":[0.12],"pos":[-0.45,-0.05,-0.38]}]"#.into()
    } else if lower.contains("sword") || lower.contains("épée") || lower.contains("blade") {
        r#"[{"shape":"Box","name":"Blade","scale":[0.03,0.6,0.01],"pos":[0,0.65,0]},{"shape":"Cylinder","name":"Handle","params":[0.025,0.12],"pos":[0,-0.07,0]},{"shape":"Box","name":"Guard","scale":[0.15,0.02,0.03],"pos":[0,0.05,0]},{"shape":"Sphere","name":"Pommel","params":[0.04],"pos":[0,-0.22,0]}]"#.into()
    } else if lower.contains("mushroom") || lower.contains("champignon") {
        r#"[{"shape":"Cylinder","name":"Stem","params":[0.08,0.3],"pos":[0,-0.1,0]},{"shape":"Sphere","name":"Cap","params":[0.35],"pos":[0,0.3,0]},{"shape":"Box","name":"CapCut","scale":[0.5,0.25,0.5],"pos":[0,0.08,0],"op":"Subtract"}]"#.into()
    } else {
        // Default: robot
        r#"[{"shape":"Box","name":"Torso","scale":[0.25,0.3,0.15],"pos":[0,0.2,0]},{"shape":"Sphere","name":"Head","params":[0.18],"pos":[0,0.65,0]},{"shape":"Capsule","name":"ArmL","params":[0.05,0.2],"pos":[-0.35,0.25,0],"rot":[0,0,-15]},{"shape":"Capsule","name":"ArmR","params":[0.05,0.2],"pos":[0.35,0.25,0],"rot":[0,0,15]},{"shape":"Capsule","name":"LegL","params":[0.06,0.25],"pos":[-0.12,-0.35,0]},{"shape":"Capsule","name":"LegR","params":[0.06,0.25],"pos":[0.12,-0.35,0]}]"#.into()
    }
}

// ====================================================================
// LLM JSON sanitizer
// ====================================================================

/// Strip markdown fences, trailing prose, and other LLM artifacts from JSON.
pub fn sanitize_llm_json(raw: &str) -> String {
    let mut s = raw.trim().to_string();
    // Strip ```json ... ``` or ``` ... ``` fences
    if s.starts_with("```") {
        // Remove opening fence line
        if let Some(nl) = s.find('\n') {
            s = s[nl + 1..].to_string();
        }
        // Remove closing fence
        if let Some(pos) = s.rfind("```") {
            s = s[..pos].to_string();
        }
        s = s.trim().to_string();
    }
    // Extract the outermost JSON array if surrounded by prose
    if let (Some(start), Some(end)) = (s.find('['), s.rfind(']')) {
        if start <= end {
            s = s[start..=end].to_string();
        }
    }
    // Remove trailing commas before ] (common LLM mistake)
    while s.contains(",]") {
        s = s.replace(",]", "]");
    }
    // Remove trailing commas before } (common LLM mistake)
    while s.contains(",}") {
        s = s.replace(",}", "}");
    }
    s
}

// ====================================================================
// JSON Parser (hand-rolled, zero serde dependency)
// ====================================================================
pub fn parse_agent_json(json: &str) -> Result<Vec<SdfPrimitive>, String> {
    let sanitized = sanitize_llm_json(json);
    let trimmed = sanitized.trim();
    if !trimmed.starts_with('[') || !trimmed.ends_with(']') {
        return Err("Expected JSON array".into());
    }
    let mut prims = Vec::new();
    let inner = &trimmed[1..trimmed.len()-1];
    for obj_str in split_json_objects(inner) {
        if obj_str.trim().is_empty() { continue; }
        let def = parse_object(obj_str.trim())?;
        prims.push(def_to_primitive(def)?);
    }
    if prims.is_empty() { return Err("No primitives in JSON".into()); }
    Ok(prims)
}

struct PrimDef { shape: String, name: String, scale: [f32;3], params: Vec<f32>, pos: [f32;3], rot: [f32;3], op: String, k: f32 }

fn split_json_objects(s: &str) -> Vec<String> {
    let mut objects = Vec::new();
    let mut depth = 0i32; let mut start = 0;
    for (i, ch) in s.char_indices() {
        match ch {
            '{' => { if depth == 0 { start = i; } depth += 1; }
            '}' => { depth -= 1; if depth == 0 { objects.push(s[start..=i].to_string()); } }
            _ => {}
        }
    }
    objects
}

fn parse_object(s: &str) -> Result<PrimDef, String> {
    let mut d = PrimDef { shape:"Box".into(), name:"Object".into(), scale:[0.3,0.3,0.3], params:vec![], pos:[0.0;3], rot:[0.0;3], op:"Union".into(), k:0.15 };
    if let Some(v) = extract_string(s,"shape") { d.shape=v; }
    if let Some(v) = extract_string(s,"name") { d.name=v; }
    if let Some(v) = extract_string(s,"op") { d.op=v; }
    if let Some(v) = extract_f32(s,"k") { d.k=v; }
    if let Some(a) = extract_array(s,"scale") { if a.len()>=3 { d.scale=[a[0],a[1],a[2]]; } }
    if let Some(a) = extract_array(s,"pos") { if a.len()>=3 { d.pos=[a[0],a[1],a[2]]; } }
    if let Some(a) = extract_array(s,"rot") { if a.len()>=3 { d.rot=[a[0],a[1],a[2]]; } }
    if let Some(a) = extract_array(s,"params") { d.params=a; }
    Ok(d)
}

fn extract_string(json: &str, key: &str) -> Option<String> {
    let pat = format!("\"{}\"", key);
    let rest = &json[json.find(&pat)? + pat.len()..];
    let after = rest[rest.find(':')?+1..].trim_start();
    if !after.starts_with('"') { return None; }
    let end = after[1..].find('"')?;
    Some(after[1..1+end].to_string())
}

fn extract_array(json: &str, key: &str) -> Option<Vec<f32>> {
    let pat = format!("\"{}\"", key);
    let rest = &json[json.find(&pat)? + pat.len()..];
    let bs = rest.find('[')?; let be = rest.find(']')?;
    let nums: Vec<f32> = rest[bs+1..be].split(',').filter_map(|s| s.trim().parse().ok()).collect();
    if nums.is_empty() { None } else { Some(nums) }
}

fn extract_f32(json: &str, key: &str) -> Option<f32> {
    let pat = format!("\"{}\"", key);
    let rest = &json[json.find(&pat)? + pat.len()..];
    let after = rest[rest.find(':')?+1..].trim_start();
    after.split(|c: char| c == ',' || c == '}').next()?.trim().parse().ok()
}

/// Clamp a value to [lo, hi], logging a warning if clamping changes the value.
fn clamp_agent_f32(val: f32, lo: f32, hi: f32, label: &str) -> f32 {
    let clamped = val.clamp(lo, hi);
    if clamped != val {
        eprintln!("[Agent] WARNING: clamped {} from {:.4} to {:.4} (range [{:.4}, {:.4}])", label, val, clamped, lo, hi);
    }
    clamped
}

fn def_to_primitive(def: PrimDef) -> Result<SdfPrimitive, String> {
    // Clamp LLM-generated positions to [-1000, 1000] and dimensions/radii to [0.001, 5.0]
    let pos = [
        clamp_agent_f32(def.pos[0], -1000.0, 1000.0, "pos.x"),
        clamp_agent_f32(def.pos[1], -1000.0, 1000.0, "pos.y"),
        clamp_agent_f32(def.pos[2], -1000.0, 1000.0, "pos.z"),
    ];
    let scale = [
        clamp_agent_f32(def.scale[0], 0.001, 5.0, "scale.x"),
        clamp_agent_f32(def.scale[1], 0.001, 5.0, "scale.y"),
        clamp_agent_f32(def.scale[2], 0.001, 5.0, "scale.z"),
    ];
    let params: Vec<f32> = def.params.iter().enumerate()
        .map(|(i, &v)| clamp_agent_f32(v, 0.001, 5.0, &format!("params[{}]", i)))
        .collect();
    let k = clamp_agent_f32(def.k, 0.0, 0.5, "blend_k");

    let shape = match def.shape.as_str() {
        "Sphere" => SdfShape::Sphere { radius: params.first().copied().unwrap_or(scale[0]) },
        "Box" => SdfShape::Box { half_extents: Vec3::new(scale[0], scale[1], scale[2]), bevel: BoxBevel::default() },
        "Cylinder" => SdfShape::Cylinder { radius: params.first().copied().unwrap_or(scale[0]), half_height: params.get(1).copied().unwrap_or(scale[1]) },
        "Capsule" => SdfShape::Capsule { radius: params.first().copied().unwrap_or(scale[0]), half_height: params.get(1).copied().unwrap_or(scale[1]) },
        "Cone" => SdfShape::Cone { radius: params.first().copied().unwrap_or(scale[0]), half_height: params.get(1).copied().unwrap_or(scale[1]) },
        "Torus" => SdfShape::Torus { major_radius: params.first().copied().unwrap_or(scale[0]), minor_radius: params.get(1).copied().unwrap_or(0.1) },
        other => return Err(format!("Unknown shape: {}", other)),
    };
    let operation = match def.op.as_str() {
        "Union"          | "SmoothUnion"    => BooleanOp::Union { k },
        "Subtract"       | "SmoothSubtract" => BooleanOp::Subtract { k },
        "Intersect"                         => BooleanOp::Intersect,
        _                                   => BooleanOp::Union { k },
    };
    let mut prim = SdfPrimitive::new(def.name, shape, Vec3::new(pos[0], pos[1], pos[2]));
    prim.rotation_deg = Vec3::new(def.rot[0], def.rot[1], def.rot[2]);
    prim.operation = operation;
    Ok(prim)
}

// ====================================================================
// Scene Export to JSON (for building LoRA training datasets)
// ====================================================================
pub fn scene_to_json(primitives: &[SdfPrimitive], description: &str) -> String {
    let mut out = String::from("{\n");
    out.push_str(&format!("  \"description\": \"{}\",\n", escape_json_string(description)));
    out.push_str("  \"primitives\": [\n");
    for (i, p) in primitives.iter().enumerate() {
        let (shape_name, params, scale) = match &p.shape {
            SdfShape::Sphere { radius } => ("Sphere", format!("[{:.3}]", radius), None),
            SdfShape::Box { half_extents, .. } => ("Box", String::new(), Some(format!("[{:.3},{:.3},{:.3}]", half_extents.x, half_extents.y, half_extents.z))),
            SdfShape::Cylinder { radius, half_height } => ("Cylinder", format!("[{:.3},{:.3}]", radius, half_height), None),
            SdfShape::Capsule { radius, half_height } => ("Capsule", format!("[{:.3},{:.3}]", radius, half_height), None),
            SdfShape::Cone { radius, half_height } => ("Cone", format!("[{:.3},{:.3}]", radius, half_height), None),
            SdfShape::Torus { major_radius, minor_radius } => ("Torus", format!("[{:.3},{:.3}]", major_radius, minor_radius), None),
            SdfShape::Stair { half_extents: he, num_steps } => ("Stair", format!("[{:.3},{:.3},{:.3},{:.3}]", he.x, he.y, he.z, num_steps), None),
            SdfShape::SpiralStair { radius, half_height, num_steps, step_width } => ("SpiralStair", format!("[{:.3},{:.3},{:.3},{:.3}]", radius, half_height, num_steps, step_width), None),
            SdfShape::Billboard { width, height, face_camera } => ("Billboard", format!("[{:.3},{:.3},{:.3}]", width, height, if *face_camera {1.0} else {0.0}), None),
            SdfShape::CurveExtrude { profile, half_depth, .. } => ("CurveExtrude", format!("[{},{:.3}]", profile.verts.len(), half_depth), None),
            SdfShape::MeshVolume { grid, .. } => ("MeshVolume", format!("[{}]", grid.res), None),
            SdfShape::HalfSpace => ("HalfSpace", String::new(), None),
            SdfShape::SpinePipe { points, radius, closed, .. } => (
                "SpinePipe",
                format!("[{},{:.3},{}]", points.len(), radius, if *closed { 1 } else { 0 }),
                None,
            ),
        };
        let op_str = if p.operation.is_additive() { "Union" } else if matches!(p.operation, BooleanOp::Intersect) { "Intersect" } else { "Subtract" };
        let op_k: Option<f32> = match &p.operation {
            BooleanOp::Intersect => None,
            _ => Some(p.operation.blend_k()),
        };
        out.push_str(&format!("    {{\"shape\":\"{}\",\"name\":\"{}\"", shape_name, escape_json_string(&p.name)));
        if !params.is_empty() { out.push_str(&format!(",\"params\":{}", params)); }
        if let Some(s) = scale { out.push_str(&format!(",\"scale\":{}", s)); }
        out.push_str(&format!(",\"pos\":[{:.3},{:.3},{:.3}]", p.position.x, p.position.y, p.position.z));
        if p.rotation_deg != Vec3::ZERO { out.push_str(&format!(",\"rot\":[{:.1},{:.1},{:.1}]", p.rotation_deg.x, p.rotation_deg.y, p.rotation_deg.z)); }
        if i > 0 {
            out.push_str(&format!(",\"op\":\"{}\"", op_str));
            if let Some(k) = op_k { out.push_str(&format!(",\"k\":{:.3}", k)); }
        }
        out.push('}');
        if i < primitives.len()-1 { out.push(','); }
        out.push('\n');
    }
    out.push_str("  ]\n}\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test_parse_ak47() { let json = mock_generate("AK-47"); let prims = parse_agent_json(&json).unwrap(); assert!(prims.len() >= 5); }
    #[test]
    fn test_extract_json_from_markdown() {
        let text = "Sure!\n```json\n[{\"shape\":\"Box\",\"name\":\"T\",\"scale\":[0.5,0.5,0.5],\"pos\":[0,0,0]}]\n```";
        assert!(extract_json_array(text).is_some());
    }
    #[test]
    fn test_escape() { let s = escape_json_string("He said \"hi\"\nbye"); assert!(s.contains("\\n")); assert!(s.contains("\\\"")); }
    #[test]
    fn test_sanitize_fences() {
        let raw = "Here is the result:\n```json\n[{\"shape\":\"Box\"}]\n```\nHope this helps!";
        let s = sanitize_llm_json(raw);
        assert!(s.starts_with('['));
        assert!(s.ends_with(']'));
    }
    #[test]
    fn test_sanitize_trailing_commas() {
        let raw = r#"[{"shape":"Box",},{"shape":"Sphere",},]"#;
        let s = sanitize_llm_json(raw);
        assert!(!s.contains(",]"));
        assert!(!s.contains(",}"));
    }
}
