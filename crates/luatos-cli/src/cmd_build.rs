use crate::{event, OutputFormat};

pub fn cmd_build_luac(src_dirs: &[String], output: &str, bitw: u32, format: &OutputFormat) -> anyhow::Result<()> {
    let out_path = std::path::Path::new(output);
    let mut total_files = Vec::new();

    for src in src_dirs {
        let src_path = std::path::Path::new(src);
        anyhow::ensure!(src_path.is_dir(), "Source directory not found: {src}");
        let files = luatos_luadb::build::compile_lua_dir(src_path, out_path, bitw, true)?;
        total_files.extend(files);
    }

    match format {
        OutputFormat::Text => {
            println!("Compiled {} files (bitw={bitw})", total_files.len());
            for f in &total_files {
                println!("  {}", f.display());
            }
        }
        OutputFormat::Json | OutputFormat::Jsonl => event::emit_result(
            format,
            "build.luac",
            "ok",
            serde_json::json!({
                "count": total_files.len(),
                "files": total_files.iter().map(|f| f.display().to_string()).collect::<Vec<_>>(),
            }),
        )?,
    }
    Ok(())
}

pub fn cmd_build_filesystem(src_dirs: &[String], output: &str, use_luac: bool, bitw: u32, bkcrc: bool, format: &OutputFormat) -> anyhow::Result<()> {
    let paths: Vec<std::path::PathBuf> = src_dirs.iter().map(std::path::PathBuf::from).collect();
    let path_refs: Vec<&std::path::Path> = paths.iter().map(|p| p.as_path()).collect();

    for p in &path_refs {
        anyhow::ensure!(p.is_dir(), "Source directory not found: {}", p.display());
    }

    let image = luatos_luadb::build::build_script_image(&path_refs, use_luac, bitw, bkcrc, true)?;

    let out_path = std::path::Path::new(output);
    if let Some(parent) = out_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(out_path, &image)?;

    match format {
        OutputFormat::Text => {
            println!("Built filesystem image: {output}");
            println!("  Size:   {} bytes ({:.1} KB)", image.len(), image.len() as f64 / 1024.0);
            println!("  Luac:   {use_luac}");
            println!("  BK CRC: {bkcrc}");
        }
        OutputFormat::Json | OutputFormat::Jsonl => event::emit_result(
            format,
            "build.filesystem",
            "ok",
            serde_json::json!({
                "output": output,
                "size": image.len(),
                "use_luac": use_luac,
                "bkcrc": bkcrc,
            }),
        )?,
    }
    Ok(())
}
