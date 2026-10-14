# AI Material + Node Workflow (Phase 1 shipped · Phase 2 plan)

**Date:** 2026-07-22  
**Status:** Phase 1 implemented · Phase 2 designed

## Owner needs

1. **Better resolution** — soft 512px results looked bad  
2. **Regenerate with material mask** — prompt for “material one” only  
3. **Prompt by object** — Mesh-01 leather strap, Mesh-02 steel shaft  
4. **Node-based workflow** — AI slots feed the material / shader graph  

## Phase 1 (shipped)

### Server (`ai_backend/server/texture_server.py`)

| Preset | Size | Steps | Maps |
|--------|------|-------|------|
| preview | 512 | 12 | derived |
| standard | **1024** | 28 | derived |
| final | **1536** | 40 | all SD |
| ultra | **2048** | 50 | all SD |

- Max size raised **1024 → 2048**  
- `/generate_map` accepts `mask_b64` + `base_png_b64` for regional material regen  
- `/generate_multi` — batch per-object targets (results under `generated_textures/by_object/`)  

### App (AI Texture window)

- Quality presets updated; **default = Final (1536)**  
- Advanced sizes: 512 … 2048  
- **Per-object prompts** panel: pull selection → `mesh-01` / `mesh-02` slots with individual prompts  
- **Generate one PBR set per object** checkbox  
- **Material mask regen**: pick map + material prompt → regenerate that map  

### State (`AiObjectPromptSlot`)

```text
id: "mesh-01"
name: "Leather Strap"
prompt: "worn brown leather, strap grain"
node_id: Some(NodeId)
```

These slots are the **bridge to the node graph** (Phase 2).

## Phase 2 (next — node graph)

1. **AI Material Source node** in shader_nodes  
   - Inputs: none (or mask TextureSample)  
   - Params: `slot_id` / prompt / quality  
   - Outputs: albedo / normal / rough / metal / height map handles  

2. **On Apply**  
   - Write generated maps into `NodePayload::Material` map slots (already on graph materials)  
   - Bind slot → selected mesh material assignment  

3. **Mask from graph**  
   - Wire a procedural mask (or metalness) into AI regen as `mask_b64`  

4. **Multi-result UI**  
   - Browser of per-object sets; drag onto material nodes  

## Usage (today)

1. Restart texture server (`python texture_server.py`) so presets reload  
2. Open AI Texture → choose **Final** or **Ultra**  
3. Select meshes → **From selection** → fill prompts (leather / steel)  
4. Enable **Generate one PBR set per object** → Generate  
5. Or single prompt at high res; use **Material mask regen** to rework one map  

## Notes

- Ultra 2048 + all maps is VRAM/time heavy (minutes on mid GPUs).  
- Multi-object currently shows the **first** target in the main preview; others land on disk under `by_object/` until Phase 2 multi-result UI.  
- Node graph visibility for maps still follows MATERIALS_UX / PROCEDURAL_TEXTURING_DESIGN (PR-VIS).  
