pub const NATIVE_RT_ACCELERATION_GROUP: u32 = 1;
pub const NATIVE_RT_ACCELERATION_BINDING: u32 = 0;

pub fn build_native_rt_wgsl(triangle_shader: &str) -> Result<String, String> {
    let hooks = ["pt_trace_scene", "pt_occluded"];
    let mut shader = triangle_shader.to_owned();
    for hook in hooks {
        let definition = format!("fn {hook}(");
        let replacement = format!("fn {hook}_software(");
        if shader.matches(&definition).count() != 1 || shader.contains(&replacement) {
            return Err(format!("native RT requires exactly one unmodified {hook} definition"));
        }
        shader = shader.replacen(&definition, &replacement, 1);
    }
    if shader.contains("var pt_hw_accel:") {
        return Err("native RT acceleration binding already exists".into());
    }
    shader.push('\n');
    shader.push_str(include_str!("native_rt_traversal.wgsl"));
    Ok(shader)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retains_entire_material_and_integrator_source() {
        let original = include_str!("../../../graphics/src/pathtrace_tri_entry.wgsl");
        let adapted = build_native_rt_wgsl(original).unwrap();
        let retained = adapted.strip_suffix(include_str!("native_rt_traversal.wgsl"))
            .unwrap().strip_suffix('\n').unwrap()
            .replace("fn pt_trace_scene_software(", "fn pt_trace_scene(")
            .replace("fn pt_occluded_software(", "fn pt_occluded(");
        assert_eq!(retained, original);
        assert!(adapted.contains("fn pt_hit_material("));
        assert!(adapted.contains("fn pt_apply_normal_maps("));
        assert_eq!(adapted.matches("fn pt_trace_scene(").count(), 1);
        assert_eq!(adapted.matches("fn pt_occluded(").count(), 1);
        assert!(adapted.contains(&format!("@group({NATIVE_RT_ACCELERATION_GROUP}) @binding({NATIVE_RT_ACCELERATION_BINDING})")));
    }

    #[test]
    fn rejects_missing_duplicate_and_already_adapted_hooks() {
        let original = include_str!("../../../graphics/src/pathtrace_tri_entry.wgsl");
        assert!(build_native_rt_wgsl("").is_err());
        assert!(build_native_rt_wgsl(&format!("{original}\nfn pt_occluded(" )).is_err());
        assert!(build_native_rt_wgsl(&original.replace("fn pt_trace_scene(", "fn gone(")).is_err());
        assert!(build_native_rt_wgsl(&build_native_rt_wgsl(original).unwrap()).is_err());
    }
}
