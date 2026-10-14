#[path = "../../../src/native_rt_shader.rs"]
mod native_rt_shader;

fn main() -> Result<(), String> {
    let mut args = std::env::args_os().skip(1);
    let input = args.next().ok_or("expected assembled triangle WGSL path")?;
    let output = args.next().ok_or("expected compiled shader output path")?;
    let source = std::fs::read_to_string(input).map_err(|error| error.to_string())?;
    let source = native_rt_shader::build_native_rt_wgsl(&source)?;
    let module = naga::front::wgsl::parse_str(&source)
        .map_err(|error| error.emit_to_string(&source))?;
    let info = naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(), naga::valid::Capabilities::all(),
    ).validate(&module).map_err(|error| error.emit_to_string(&source))?;
    let options = naga::back::spv::Options {
        lang_version: (1, 4),
        flags: naga::back::spv::WriterFlags::empty(),
        ..Default::default()
    };
    let words = naga::back::spv::write_vec(&module, &info, &options, None)
        .map_err(|error| error.to_string())?;
    let bytes: Vec<u8> = words.iter().flat_map(|word| word.to_le_bytes()).collect();
    std::fs::write(output, &bytes).map_err(|error| error.to_string())?;
    println!("Naga 25.0.1 compiled complete native RT WGSL: {} bytes, {} entry points, AS group {} binding {}",
        bytes.len(), module.entry_points.len(),
        native_rt_shader::NATIVE_RT_ACCELERATION_GROUP,
        native_rt_shader::NATIVE_RT_ACCELERATION_BINDING);
    Ok(())
}
