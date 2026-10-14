// core_engine/app/src/material_dict.rs
// Smart Prompt — local material dictionary + fuzzy search.
// No server call required; entire dictionary compiled into the binary.

// ─────────────────────────────────────────────────────────────────────────────
// Data types
// ─────────────────────────────────────────────────────────────────────────────

pub struct MaterialEntry {
    /// Short name shown in dropdown: "rusted steel"
    pub name: &'static str,
    /// Gray hint line under the name: "corroded, flaking orange oxide"
    pub hint: &'static str,
    /// Full text placed into the prompt field on selection
    pub prompt_fragment: &'static str,
    /// Category used for the browser panel
    pub category: &'static str,
    /// Extra keywords for fuzzy matching (not shown in UI)
    pub tags: &'static [&'static str],
}

// ─────────────────────────────────────────────────────────────────────────────
// Fuzzy search
// ─────────────────────────────────────────────────────────────────────────────

/// Returns a score in 0.0..=1.0. 0.0 means no match.
fn fuzzy_score(query: &str, target: &str) -> f32 {
    // Fast path — empty
    if query.is_empty() { return 0.0; }

    let q = query.to_ascii_lowercase();
    let t = target.to_ascii_lowercase();

    // 1.0  — target starts with entire query
    if t.starts_with(q.as_str()) { return 1.0; }
    // 0.85 — target contains entire query as substring
    if t.contains(q.as_str()) { return 0.85; }

    // Word-boundary match: every query word is a prefix of some target word
    let q_words: Vec<&str> = q.split_whitespace().collect();
    let t_words: Vec<&str> = t.split_whitespace().collect();
    if !q_words.is_empty() {
        let matched = q_words.iter().filter(|qw| {
            t_words.iter().any(|tw| tw.starts_with(*qw))
        }).count();
        if matched == q_words.len() { return 0.70; }
        if matched > 0 {
            return 0.40 * matched as f32 / q_words.len() as f32;
        }
    }
    0.0
}

/// Returns up to `max_results` entries with a score above 0, sorted best-first.
pub fn search_materials(query: &str, max_results: usize) -> Vec<&'static MaterialEntry> {
    if query.trim().is_empty() { return vec![]; }

    let mut scored: Vec<(&'static MaterialEntry, f32)> = MATERIAL_DICTIONARY
        .iter()
        .filter_map(|e| {
            let mut best = fuzzy_score(query, e.name);
            // Tags are also searched, slightly lower priority
            for tag in e.tags {
                let s = fuzzy_score(query, tag) * 0.9;
                if s > best { best = s; }
            }
            // Category search at lower priority
            let cs = fuzzy_score(query, e.category) * 0.55;
            if cs > best { best = cs; }
            if best > 0.0 { Some((e, best)) } else { None }
        })
        .collect();

    // Sort best-first (stable for equal scores → dictionary order)
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    scored.truncate(max_results);
    scored.into_iter().map(|(e, _)| e).collect()
}

/// Category accent colour for the browser grid.
pub fn category_color(category: &str) -> egui::Color32 {
    use egui::Color32;
    match category {
        "Metal"     => Color32::from_rgb(160, 175, 200),
        "Wood"      => Color32::from_rgb(160, 120,  60),
        "Stone"     => Color32::from_rgb(155, 140, 120),
        "Organic"   => Color32::from_rgb(110, 175,  80),
        "Synthetic" => Color32::from_rgb( 70, 155, 200),
        "Fabric"    => Color32::from_rgb(200, 155, 100),
        "Paint"     => Color32::from_rgb(200,  70,  70),
        "Sci-Fi"    => Color32::from_rgb(180,  80, 250),
        _           => Color32::from_rgb(140, 140, 140),
    }
}

/// All distinct category names, in display order.
pub const CATEGORIES: &[&str] = &[
    "Metal", "Wood", "Stone", "Organic", "Synthetic", "Fabric", "Paint", "Sci-Fi",
];

// ─────────────────────────────────────────────────────────────────────────────
// The dictionary  (~130 entries, ships compiled into the binary)
// ─────────────────────────────────────────────────────────────────────────────
use eframe::egui;

pub static MATERIAL_DICTIONARY: &[MaterialEntry] = &[
    // ── Metal — Clean ──────────────────────────────────────────────────────
    MaterialEntry {
        name: "clean steel", hint: "polished industrial steel, minimal wear",
        prompt_fragment: "clean polished steel surface, subtle machining marks, industrial metal",
        category: "Metal", tags: &["steel", "metal", "clean", "polished", "shiny"],
    },
    MaterialEntry {
        name: "brushed steel", hint: "linear grain, directional brush marks",
        prompt_fragment: "brushed stainless steel, fine directional linear scratches, satin sheen",
        category: "Metal", tags: &["steel", "brushed", "satin", "linear", "metal"],
    },
    MaterialEntry {
        name: "polished steel", hint: "mirror finish, highly reflective",
        prompt_fragment: "mirror-polished steel, highly reflective, silver metallic",
        category: "Metal", tags: &["steel", "polished", "mirror", "reflective", "shiny"],
    },
    MaterialEntry {
        name: "cast iron", hint: "rough granular, dark grey, heavy",
        prompt_fragment: "cast iron surface, rough granular texture, dark grey, heavy industrial",
        category: "Metal", tags: &["iron", "cast", "rough", "dark", "grey", "industrial"],
    },
    MaterialEntry {
        name: "galvanized steel", hint: "zinc crystalline spangle pattern, matte silver",
        prompt_fragment: "galvanized steel, zinc spangle crystalline pattern, matte silver coating",
        category: "Metal", tags: &["galvanized", "zinc", "spangle", "crystalline", "silver"],
    },
    MaterialEntry {
        name: "brushed aluminum", hint: "anodized, directional brush marks, satin",
        prompt_fragment: "brushed aluminum surface, fine directional linear scratches, anodized silver satin",
        category: "Metal", tags: &["aluminum", "brushed", "anodized", "silver", "satin"],
    },
    MaterialEntry {
        name: "raw aluminum", hint: "unfinished, machined marks, light grey",
        prompt_fragment: "raw aluminum surface, light grey, machined marks, low reflectivity",
        category: "Metal", tags: &["aluminum", "raw", "machined", "light", "grey"],
    },
    MaterialEntry {
        name: "chrome", hint: "mirror polish, highly reflective, polishing swirls",
        prompt_fragment: "chrome plated mirror surface, ultra reflective, faint polishing swirls",
        category: "Metal", tags: &["chrome", "mirror", "reflective", "shiny", "plated"],
    },
    MaterialEntry {
        name: "copper", hint: "warm orange metal, high specular",
        prompt_fragment: "copper metal surface, warm orange-brown tones, high specular",
        category: "Metal", tags: &["copper", "orange", "warm", "metallic"],
    },
    MaterialEntry {
        name: "oxidized copper", hint: "blue-green patina, verdigris texture",
        prompt_fragment: "heavily oxidized copper, blue-green patina, verdigris texture, aged",
        category: "Metal", tags: &["copper", "oxidized", "patina", "verdigris", "green", "aged"],
    },
    MaterialEntry {
        name: "brass", hint: "yellow-gold alloy, tarnish, warm reflections",
        prompt_fragment: "polished brass, warm golden yellow, high specular, subtle tarnish",
        category: "Metal", tags: &["brass", "gold", "yellow", "tarnish", "warm"],
    },
    MaterialEntry {
        name: "aged brass", hint: "dark golden brown, tarnished, patina spots",
        prompt_fragment: "aged brass surface, dark golden brown, tarnished, patina spots and streaks",
        category: "Metal", tags: &["brass", "aged", "tarnish", "dark", "patina"],
    },
    MaterialEntry {
        name: "bronze", hint: "brownish-grey alloy, antique finish",
        prompt_fragment: "dark bronze alloy surface, brownish-grey, antique classical finish",
        category: "Metal", tags: &["bronze", "brown", "antique", "alloy"],
    },
    MaterialEntry {
        name: "gold", hint: "vivid yellow, mirror-like reflective",
        prompt_fragment: "gold metal surface, vivid warm yellow, mirror-like reflective finish",
        category: "Metal", tags: &["gold", "yellow", "mirror", "reflective", "precious"],
    },
    MaterialEntry {
        name: "silver", hint: "bright white-grey, high reflectivity",
        prompt_fragment: "pure silver surface, bright white-grey, high reflectivity",
        category: "Metal", tags: &["silver", "white", "bright", "reflective", "precious"],
    },
    MaterialEntry {
        name: "titanium", hint: "dark silver-blue grey, anodized iridescence",
        prompt_fragment: "titanium metal surface, dark silver-blue grey, subtle purple iridescence",
        category: "Metal", tags: &["titanium", "dark", "grey", "iridescent", "anodized"],
    },
    MaterialEntry {
        name: "gunmetal", hint: "dark blue-grey, matte-satin military finish",
        prompt_fragment: "gunmetal grey alloy, dark blue-grey, matte-satin military surface finish",
        category: "Metal", tags: &["gunmetal", "grey", "dark", "matte", "military"],
    },
    MaterialEntry {
        name: "nickel", hint: "bright silver, slightly warm tint, plated",
        prompt_fragment: "nickel plated surface, bright silver, slightly warm tint",
        category: "Metal", tags: &["nickel", "plated", "bright", "silver"],
    },
    // ── Metal — Worn / Corroded ────────────────────────────────────────────
    MaterialEntry {
        name: "rusted steel", hint: "corroded, flaking orange oxide, dark pitting",
        prompt_fragment: "corroded weathered steel, heavy flaking orange-brown rust, dark iron pitting",
        category: "Metal", tags: &["rust", "rusted", "corroded", "oxidized", "orange", "weathered"],
    },
    MaterialEntry {
        name: "light surface rust", hint: "thin orange film on steel, early corrosion",
        prompt_fragment: "light surface rust on steel, thin orange oxidation film, base metal still visible",
        category: "Metal", tags: &["rust", "light", "surface", "patina", "thin", "orange"],
    },
    MaterialEntry {
        name: "pitted corrosion", hint: "deep craters, dark brown, severe damage",
        prompt_fragment: "severe pitting corrosion on steel, deep craters, dark brown corroded metal",
        category: "Metal", tags: &["pitting", "corrosion", "craters", "dark", "severe"],
    },
    MaterialEntry {
        name: "galvanic corrosion", hint: "white powdery deposits on aluminum",
        prompt_fragment: "galvanic corrosion on aluminum, white powdery oxide deposits, pitting, degraded surface",
        category: "Metal", tags: &["galvanic", "corrosion", "aluminum", "white", "powder", "oxide"],
    },
    MaterialEntry {
        name: "blued steel", hint: "deep blue-black oxidized, hot blue finish",
        prompt_fragment: "hot-blued steel surface, deep blue-black oxidized finish, uniform dark coating",
        category: "Metal", tags: &["blued", "blue", "black", "oxidized", "finish"],
    },
    MaterialEntry {
        name: "heat treated steel", hint: "rainbow oxide, blue-purple-gold zones",
        prompt_fragment: "heat treated steel, rainbow oxide colors, blue-purple-gold gradient zones",
        category: "Metal", tags: &["heat", "treated", "rainbow", "iridescent", "blue", "gold"],
    },
    MaterialEntry {
        name: "weld bead", hint: "ripple pattern, heat-discolored zone around bead",
        prompt_fragment: "TIG weld bead on steel, textured ripple pattern, heat-discolored zone, dark oxide",
        category: "Metal", tags: &["weld", "bead", "tig", "ripple", "heat", "oxide"],
    },
    // ── Metal — Coatings ──────────────────────────────────────────────────
    MaterialEntry {
        name: "powder coat black", hint: "deep matte black, orange-peel micro-texture",
        prompt_fragment: "black powder coated metal, deep matte black, fine orange peel micro-texture, uniform finish",
        category: "Metal", tags: &["powder", "coat", "black", "matte", "dark"],
    },
    MaterialEntry {
        name: "powder coat white", hint: "white matte-satin, smooth coating",
        prompt_fragment: "white powder coat finish, smooth matte-satin coating, uniform coverage",
        category: "Metal", tags: &["powder", "coat", "white", "matte", "coating"],
    },
    MaterialEntry {
        name: "powder coat grey", hint: "industrial grey matte finish",
        prompt_fragment: "medium grey powder coat, industrial matte finish, uniform surface",
        category: "Metal", tags: &["powder", "coat", "grey", "matte", "industrial"],
    },
    MaterialEntry {
        name: "phosphate coat", hint: "parkerized dark grey matte, corrosion resistant",
        prompt_fragment: "phosphate parkerized finish, dark grey matte, fine crystalline texture, military",
        category: "Metal", tags: &["phosphate", "parkerized", "dark", "grey", "matte", "military"],
    },
    MaterialEntry {
        name: "anodized black", hint: "hard anodized aluminum, dark charcoal-black",
        prompt_fragment: "hard anodized aluminum, dark charcoal to black, durable matte surface",
        category: "Metal", tags: &["anodized", "aluminum", "black", "dark", "hard"],
    },
    MaterialEntry {
        name: "diamond plate", hint: "raised diamond pattern, metallic tread",
        prompt_fragment: "diamond plate aluminum tread, raised diamond pattern, metallic, worn at high points",
        category: "Metal", tags: &["diamond", "plate", "tread", "raised", "pattern", "metallic"],
    },
    MaterialEntry {
        name: "perforated metal", hint: "regular hole pattern, flat sheet",
        prompt_fragment: "perforated steel sheet, regular circular hole pattern, matte grey, industrial",
        category: "Metal", tags: &["perforated", "holes", "sheet", "pattern", "industrial"],
    },
    MaterialEntry {
        name: "worn painted metal", hint: "chipped paint revealing bare steel",
        prompt_fragment: "worn painted metal, chipped peeling paint, exposed bare steel at edges and high points",
        category: "Metal", tags: &["paint", "chipped", "worn", "peeling", "exposed", "damage"],
    },
    MaterialEntry {
        name: "battle-damaged armor", hint: "deep gouges, impact craters, scorched edges",
        prompt_fragment: "battle-damaged metal armor, deep gouges, projectile impact craters, scorched blackened edges",
        category: "Metal", tags: &["battle", "damage", "armor", "gouges", "impact", "war"],
    },
    // ── Wood ──────────────────────────────────────────────────────────────
    MaterialEntry {
        name: "oak wood", hint: "warm brown, pronounced growth rings",
        prompt_fragment: "natural oak wood grain, warm medium brown, pronounced growth rings, open grain",
        category: "Wood", tags: &["oak", "wood", "grain", "brown", "warm", "natural"],
    },
    MaterialEntry {
        name: "dark walnut", hint: "dark chocolate brown, flowing elegant grain",
        prompt_fragment: "walnut wood surface, dark chocolate brown, flowing elegant grain, polished finish",
        category: "Wood", tags: &["walnut", "wood", "dark", "chocolate", "brown", "luxury"],
    },
    MaterialEntry {
        name: "pine wood", hint: "light blond, visible knots, resinous",
        prompt_fragment: "pine wood surface, light blond yellow, visible knots, resinous grain, soft wood",
        category: "Wood", tags: &["pine", "wood", "light", "blond", "knots", "soft"],
    },
    MaterialEntry {
        name: "weathered wood", hint: "sun-bleached grey, cracked, splinters",
        prompt_fragment: "weathered gray wood, sun-bleached faded surface, deep cracks and splinters, aged outdoor timber",
        category: "Wood", tags: &["weathered", "wood", "grey", "aged", "cracked", "bleached", "old"],
    },
    MaterialEntry {
        name: "charred wood", hint: "blackened burned, deep heat cracks",
        prompt_fragment: "charred burned wood, blackened charcoal texture, deep heat cracks, shou sugi ban finish",
        category: "Wood", tags: &["charred", "burned", "black", "wood", "fire", "charcoal"],
    },
    MaterialEntry {
        name: "plywood", hint: "layered birch, pale tan, edge grain visible",
        prompt_fragment: "birch plywood surface, light tan, layered grain visible at edge, smooth face",
        category: "Wood", tags: &["plywood", "birch", "light", "tan", "layered"],
    },
    MaterialEntry {
        name: "reclaimed wood", hint: "old timber, nail holes, grey patina, paint traces",
        prompt_fragment: "reclaimed old timber, grey weathered patina, nail holes, traces of old paint, rough surface",
        category: "Wood", tags: &["reclaimed", "old", "timber", "rough", "grey", "patina", "vintage"],
    },
    // ── Stone & Concrete ──────────────────────────────────────────────────
    MaterialEntry {
        name: "concrete raw", hint: "poured concrete, aggregate, micro-cracks",
        prompt_fragment: "raw poured concrete, visible stone aggregate, micro-cracks, porous matte grey",
        category: "Stone", tags: &["concrete", "cement", "grey", "urban", "industrial", "matte"],
    },
    MaterialEntry {
        name: "polished concrete", hint: "smooth grey, subtle aggregate, low sheen",
        prompt_fragment: "polished concrete floor, smooth grey, subtle aggregate visible, low sheen",
        category: "Stone", tags: &["concrete", "polished", "smooth", "grey", "floor"],
    },
    MaterialEntry {
        name: "marble white", hint: "white with grey veining, polished crystalline",
        prompt_fragment: "white marble, elegant grey veining patterns, polished crystalline finish",
        category: "Stone", tags: &["marble", "white", "veins", "polished", "luxury", "stone"],
    },
    MaterialEntry {
        name: "granite", hint: "speckled mineral grains, hard polished",
        prompt_fragment: "polished granite, speckled mineral grains, black grey and white flecks, hard stone",
        category: "Stone", tags: &["granite", "speckled", "stone", "hard", "mineral"],
    },
    MaterialEntry {
        name: "sandstone", hint: "warm sandy beige, fine grained, layered",
        prompt_fragment: "sandstone, layered sedimentary texture, warm sandy beige color, fine grainy",
        category: "Stone", tags: &["sandstone", "sandy", "beige", "warm", "sedimentary"],
    },
    MaterialEntry {
        name: "slate", hint: "dark grey, flaky layered, rough surface",
        prompt_fragment: "dark grey slate stone, layered flaky surface, rough, natural cleft texture",
        category: "Stone", tags: &["slate", "dark", "grey", "layered", "rough"],
    },
    MaterialEntry {
        name: "brick", hint: "red fired brick, mortar joints",
        prompt_fragment: "red fired brick, rough rectangular surface, mortar joints visible, weathered",
        category: "Stone", tags: &["brick", "red", "mortar", "fired", "wall"],
    },
    MaterialEntry {
        name: "cobblestone", hint: "rounded irregular stones, weathered",
        prompt_fragment: "irregular cobblestone pavement, rounded grey stones, weathered, worn by foot traffic",
        category: "Stone", tags: &["cobblestone", "rounded", "stones", "street", "weathered"],
    },
    MaterialEntry {
        name: "mossy stone", hint: "old stone, green moss and lichen growth",
        prompt_fragment: "old stone with green moss patches and lichen, damp weathered, organic growth",
        category: "Stone", tags: &["moss", "stone", "green", "lichen", "damp", "old", "organic"],
    },
    MaterialEntry {
        name: "asphalt", hint: "dark road surface, coarse aggregate, rough",
        prompt_fragment: "dark asphalt road surface, coarse aggregate, black-grey, rough wear marks",
        category: "Stone", tags: &["asphalt", "road", "dark", "aggregate", "rough"],
    },
    // ── Organic ───────────────────────────────────────────────────────────
    MaterialEntry {
        name: "leather brown", hint: "organic pore pattern, natural grain",
        prompt_fragment: "natural brown leather, organic pore pattern, subtle creases, warm grain texture",
        category: "Organic", tags: &["leather", "brown", "organic", "pores", "natural"],
    },
    MaterialEntry {
        name: "worn leather", hint: "aged cracked, darkened creases, distressed",
        prompt_fragment: "aged worn leather, deep creases, cracked patina, darkened at folds, distressed vintage",
        category: "Organic", tags: &["leather", "worn", "aged", "cracked", "distressed", "vintage"],
    },
    MaterialEntry {
        name: "tree bark", hint: "deep fissures, rough ridges, natural pattern",
        prompt_fragment: "tree bark surface, deep fissures and ridges, rough layered texture, natural organic",
        category: "Organic", tags: &["bark", "tree", "rough", "fissures", "ridges", "organic"],
    },
    MaterialEntry {
        name: "bone", hint: "smooth ivory, porous structure, hard organic",
        prompt_fragment: "bone surface, smooth ivory color, porous internal structure, hard organic material",
        category: "Organic", tags: &["bone", "ivory", "porous", "organic", "white", "hard"],
    },
    MaterialEntry {
        name: "skin human", hint: "fine pores, micro-wrinkles, subsurface warmth",
        prompt_fragment: "human skin texture, fine pores, subtle micro-wrinkles, subsurface warmth, organic",
        category: "Organic", tags: &["skin", "human", "pores", "organic", "flesh"],
    },
    // ── Synthetic ─────────────────────────────────────────────────────────
    MaterialEntry {
        name: "carbon fiber", hint: "diagonal twill weave, glossy resin coat",
        prompt_fragment: "carbon fiber weave, diagonal twill pattern, glossy resin coating, black blue tint",
        category: "Synthetic", tags: &["carbon", "fiber", "woven", "glossy", "black", "composite"],
    },
    MaterialEntry {
        name: "carbon fiber matte", hint: "dry weave, no gloss, flat black",
        prompt_fragment: "matte carbon fiber weave, black, no gloss coat, dry composite surface",
        category: "Synthetic", tags: &["carbon", "fiber", "matte", "black", "flat", "dry"],
    },
    MaterialEntry {
        name: "kevlar weave", hint: "yellow-gold aramid fiber, protective",
        prompt_fragment: "kevlar aramid fiber weave, distinctive yellow-gold color, tight protective fabric",
        category: "Synthetic", tags: &["kevlar", "aramid", "yellow", "weave", "tactical"],
    },
    MaterialEntry {
        name: "rubber matte", hint: "fine grain, matte black, soft appearance",
        prompt_fragment: "vulcanized rubber surface, fine consistent grain, matte black, industrial grip",
        category: "Synthetic", tags: &["rubber", "matte", "black", "soft", "grip"],
    },
    MaterialEntry {
        name: "silicone", hint: "translucent, slightly glossy, flexible",
        prompt_fragment: "silicone surface, translucent frosted, slightly glossy, flexible smooth texture",
        category: "Synthetic", tags: &["silicone", "translucent", "glossy", "flexible", "smooth"],
    },
    MaterialEntry {
        name: "plastic glossy", hint: "injection moulded, smooth, high gloss",
        prompt_fragment: "glossy plastic surface, smooth injection moulded finish, reflective polymer",
        category: "Synthetic", tags: &["plastic", "glossy", "smooth", "polymer", "shiny"],
    },
    MaterialEntry {
        name: "abs plastic", hint: "grey matte, injection moulded, no sheen",
        prompt_fragment: "grey ABS plastic, injection moulded, matte smooth surface",
        category: "Synthetic", tags: &["abs", "plastic", "grey", "matte", "moulded"],
    },
    MaterialEntry {
        name: "hdpe plastic", hint: "white waxy, matte, low gloss",
        prompt_fragment: "white HDPE plastic, slightly waxy appearance, matte, low gloss",
        category: "Synthetic", tags: &["hdpe", "plastic", "white", "waxy", "matte"],
    },
    MaterialEntry {
        name: "fiberglass", hint: "smooth gel-coat, white-cream, slightly glossy",
        prompt_fragment: "fiberglass composite, smooth white gel-coat surface, slightly glossy, marine finish",
        category: "Synthetic", tags: &["fiberglass", "white", "gel", "coat", "smooth", "marine"],
    },
    MaterialEntry {
        name: "pcb circuit board", hint: "green soldermask, gold traces, components",
        prompt_fragment: "FR4 PCB circuit board, green soldermask, gold copper traces, solder joints visible",
        category: "Synthetic", tags: &["pcb", "circuit", "board", "green", "gold", "electronics"],
    },
    // ── Fabric ────────────────────────────────────────────────────────────
    MaterialEntry {
        name: "canvas", hint: "coarse woven cotton, visible thread crossing",
        prompt_fragment: "canvas fabric, coarse woven cotton, visible thread crossing pattern, durable",
        category: "Fabric", tags: &["canvas", "fabric", "woven", "cotton", "coarse", "cloth"],
    },
    MaterialEntry {
        name: "denim", hint: "twill weave, indigo blue, white thread beneath",
        prompt_fragment: "denim fabric, twill weave pattern, indigo blue surface, white cotton thread visible",
        category: "Fabric", tags: &["denim", "jeans", "blue", "twill", "indigo", "fabric"],
    },
    MaterialEntry {
        name: "silk", hint: "smooth lustrous, flowing light reflections",
        prompt_fragment: "silk fabric, extremely smooth lustrous sheen, flowing light reflections, delicate weave",
        category: "Fabric", tags: &["silk", "smooth", "lustrous", "shiny", "delicate", "fabric"],
    },
    MaterialEntry {
        name: "military fabric", hint: "cordura nylon, olive drab, heavy duty",
        prompt_fragment: "military tactical fabric, cordura nylon weave, olive drab green, heavy duty ripstop",
        category: "Fabric", tags: &["military", "tactical", "nylon", "olive", "cordura"],
    },
    // ── Paint ─────────────────────────────────────────────────────────────
    MaterialEntry {
        name: "matte black paint", hint: "flat zero-sheen, uniform, no specular",
        prompt_fragment: "flat matte black paint coating, zero specular, uniform zero-sheen surface",
        category: "Paint", tags: &["matte", "black", "paint", "flat", "dark"],
    },
    MaterialEntry {
        name: "gloss automotive paint", hint: "deep clearcoat, metallic flakes, mirror-like",
        prompt_fragment: "fresh automotive paint, deep clearcoat finish, metallic flakes, mirror-like depth",
        category: "Paint", tags: &["paint", "automotive", "car", "clearcoat", "metallic", "glossy"],
    },
    MaterialEntry {
        name: "primer grey", hint: "rough matte, light grey etching primer",
        prompt_fragment: "grey etching primer coating, rough matte surface, light grey, porous",
        category: "Paint", tags: &["primer", "grey", "matte", "rough", "etching"],
    },
    MaterialEntry {
        name: "hammertone paint", hint: "dimpled metallic surface, textured finish",
        prompt_fragment: "hammertone textured paint, dimpled metallic finish, industrial surface coating",
        category: "Paint", tags: &["hammertone", "textured", "dimpled", "metallic", "paint"],
    },
    MaterialEntry {
        name: "spray paint graffiti", hint: "thick acrylic layers, drip runs",
        prompt_fragment: "spray paint graffiti, thick layered acrylic, drip runs, overlapping color patches",
        category: "Paint", tags: &["spray", "graffiti", "paint", "drips", "urban", "acrylic"],
    },
    MaterialEntry {
        name: "chipped old paint", hint: "flaking layers, exposed surface beneath",
        prompt_fragment: "old chipped paint, flaking layers showing undercoat and bare surface, weathered",
        category: "Paint", tags: &["chipped", "old", "paint", "flaking", "weathered", "peeling"],
    },
    // ── Sci-Fi / Stylized ─────────────────────────────────────────────────
    MaterialEntry {
        name: "sci-fi hull panel", hint: "panel lines, hex rivets, subtle edge wear",
        prompt_fragment: "futuristic spacecraft hull panel, clean recessed panel lines, hex rivets, subtle edge wear",
        category: "Sci-Fi", tags: &["scifi", "panel", "hull", "spaceship", "futuristic", "rivets"],
    },
    MaterialEntry {
        name: "sci-fi floor grating", hint: "diamond plate grip, industrial spaceship",
        prompt_fragment: "sci-fi industrial floor grating, diamond plate grip pattern, heavy boot wear marks",
        category: "Sci-Fi", tags: &["scifi", "floor", "grating", "diamond", "plate", "industrial"],
    },
    MaterialEntry {
        name: "alien organic", hint: "biomechanical ridges, translucent membrane",
        prompt_fragment: "alien biomechanical surface, organic ridges and folds, translucent wet membrane patches",
        category: "Sci-Fi", tags: &["alien", "organic", "biomechanical", "membrane", "wet", "creature"],
    },
    MaterialEntry {
        name: "cyberpunk neon grime", hint: "grimy dark metal, neon light stains",
        prompt_fragment: "cyberpunk urban surface, grimy dark metal, neon light color stains, accumulated grime",
        category: "Sci-Fi", tags: &["cyberpunk", "neon", "grime", "urban", "dark", "dirty"],
    },
    MaterialEntry {
        name: "holographic coating", hint: "rainbow iridescent, shifting color diffraction",
        prompt_fragment: "holographic iridescent coating, rainbow color-shifting diffraction grating pattern",
        category: "Sci-Fi", tags: &["holographic", "iridescent", "rainbow", "diffraction", "shifting"],
    },
    MaterialEntry {
        name: "energy shield", hint: "translucent hex grid, glowing blue-white",
        prompt_fragment: "energy shield surface, translucent hexagonal grid, glowing blue-white, force field",
        category: "Sci-Fi", tags: &["energy", "shield", "hex", "glow", "force", "field", "blue"],
    },
    MaterialEntry {
        name: "steampunk brass pipes", hint: "Victorian riveted plates, steam stains, verdigris",
        prompt_fragment: "steampunk brass machinery, Victorian riveted plates, steam stains, green verdigris patches",
        category: "Sci-Fi", tags: &["steampunk", "brass", "victorian", "rivets", "pipes", "gears"],
    },
    MaterialEntry {
        name: "liquid metal", hint: "mercury-like, flowing reflective surface",
        prompt_fragment: "liquid metal surface, mercury-like, highly reflective flowing finish, silver",
        category: "Sci-Fi", tags: &["liquid", "metal", "mercury", "reflective", "flowing", "silver"],
    },
];
