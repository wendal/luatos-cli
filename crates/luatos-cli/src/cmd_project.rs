use std::fs;

use crate::{event, OutputFormat};

pub fn cmd_project_new(dir: &str, name: &str, chip: &str, format: &OutputFormat) -> anyhow::Result<()> {
    let dir_path = std::path::Path::new(dir);
    luatos_project::scaffold_project(dir_path, name, chip, &luatos_project::wizard::TemplateKind::HelloWorld)?;

    match format {
        OutputFormat::Text => {
            println!("Created project '{name}' for chip '{chip}' in {dir}/");
            println!("  Config:  {dir}/luatos-project.toml");
            println!("  Script:  {dir}/lua/main.lua");
            println!("  README:  {dir}/README.md");
            println!("\nNext steps:");
            println!("  cd {dir}");
            println!("  luatos-cli project build        # 使用项目配置构建（推荐）");
            println!("  # 或者手动指定参数:");
            println!("  luatos-cli build filesystem --src lua/ --output build/script.bin");
        }
        OutputFormat::Json | OutputFormat::Jsonl => event::emit_result(format, "project.new", "ok", serde_json::json!({ "name": name, "chip": chip, "dir": dir }))?,
    }
    Ok(())
}

pub fn cmd_project_info(dir: &str, format: &OutputFormat) -> anyhow::Result<()> {
    let project = luatos_project::Project::load(std::path::Path::new(dir))?;

    match format {
        OutputFormat::Text => {
            println!("Project: {}", project.project.name);
            println!("  Chip:        {}", project.project.chip);
            println!("  Version:     {}", project.project.version);
            if let Some(ref desc) = project.project.description {
                println!("  Desc:        {desc}");
            }
            println!("  Scripts:     {}", project.build.script_dirs.join(", "));
            if !project.build.script_files.is_empty() {
                println!("  Files:       {}", project.build.script_files.join(", "));
            }
            println!("  Output:      {}", project.build.output_dir);
            println!("  Use luac:    {}", project.build.use_luac);
            println!("  Bitwidth:    {}", project.build.bitw);
            println!("  Debug info:  {}", project.build.luac_debug);
            println!("  Ignore deps: {}", project.build.ignore_deps);
            println!("  soc_script:  {}", project.build.soc_script);
            println!("  Resource:    {}", project.build.resource_dir);
            if let Some(ref soc) = project.flash.soc_file {
                println!("  SOC:         {soc}");
            }
            if let Some(ref port) = project.flash.port {
                println!("  Port:        {port}");
            }
        }
        OutputFormat::Json | OutputFormat::Jsonl => event::emit_result(format, "project.info", "ok", &project)?,
    }
    Ok(())
}

pub fn cmd_project_config(dir: &str, key: Option<&str>, value: Option<&str>, format: &OutputFormat) -> anyhow::Result<()> {
    let dir_path = std::path::Path::new(dir);
    let mut project = luatos_project::Project::load(dir_path)?;

    match (key, value) {
        (None, _) => {
            // No key: show full config
            match format {
                OutputFormat::Text => {
                    let toml_str = toml::to_string_pretty(&project)?;
                    println!("{toml_str}");
                }
                OutputFormat::Json | OutputFormat::Jsonl => event::emit_result(format, "project.config", "ok", &project)?,
            }
        }
        (Some(k), None) => {
            // Key only: get value
            let val = get_config_value(&project, k)?;
            match format {
                OutputFormat::Text => println!("{val}"),
                OutputFormat::Json | OutputFormat::Jsonl => event::emit_result(format, "project.config", "ok", serde_json::json!({ "key": k, "value": val }))?,
            }
        }
        (Some(k), Some(v)) => {
            // Key + value: set
            set_config_value(&mut project, k, v)?;
            project.save(dir_path)?;
            match format {
                OutputFormat::Text => println!("Set {k} = {v}"),
                OutputFormat::Json | OutputFormat::Jsonl => event::emit_result(format, "project.config", "ok", serde_json::json!({ "key": k, "value": v }))?,
            }
        }
    }
    Ok(())
}

fn get_config_value(project: &luatos_project::Project, key: &str) -> anyhow::Result<String> {
    Ok(match key {
        "project.name" => project.project.name.clone(),
        "project.chip" => project.project.chip.clone(),
        "project.version" => project.project.version.clone(),
        "project.description" => project.project.description.clone().unwrap_or_default(),
        "build.script_dir" | "build.script_dirs" => project.build.script_dirs.join(", "),
        "build.script_files" => project.build.script_files.join(", "),
        "build.output_dir" => project.build.output_dir.clone(),
        "build.use_luac" => project.build.use_luac.to_string(),
        "build.bitw" => project.build.bitw.to_string(),
        "build.luac_debug" => project.build.luac_debug.to_string(),
        "build.ignore_deps" => project.build.ignore_deps.to_string(),
        "flash.soc_file" => project.flash.soc_file.clone().unwrap_or_default(),
        "flash.port" => project.flash.port.clone().unwrap_or_default(),
        "flash.baud" => project.flash.baud.map(|b| b.to_string()).unwrap_or_default(),
        _ => anyhow::bail!("Unknown config key: {key}"),
    })
}

fn set_config_value(project: &mut luatos_project::Project, key: &str, value: &str) -> anyhow::Result<()> {
    match key {
        "project.name" => project.project.name = value.to_string(),
        "project.chip" => project.project.chip = value.to_string(),
        "project.version" => project.project.version = value.to_string(),
        "project.description" => project.project.description = Some(value.to_string()),
        "build.script_dir" | "build.script_dirs" => {
            project.build.script_dirs = value.split(',').map(|s| s.trim().to_string()).collect();
        }
        "build.script_files" => {
            project.build.script_files = value.split(',').map(|s| s.trim().to_string()).collect();
        }
        "build.output_dir" => project.build.output_dir = value.to_string(),
        "build.use_luac" => project.build.use_luac = value.parse()?,
        "build.bitw" => project.build.bitw = value.parse()?,
        "build.luac_debug" => project.build.luac_debug = value.parse()?,
        "build.ignore_deps" => project.build.ignore_deps = value.parse()?,
        "flash.soc_file" => project.flash.soc_file = Some(value.to_string()),
        "flash.port" => project.flash.port = Some(value.to_string()),
        "flash.baud" => project.flash.baud = Some(value.parse()?),
        _ => anyhow::bail!("Unknown config key: {key}"),
    }
    Ok(())
}

pub fn cmd_project_import(file: &str, dir: &str, format: &OutputFormat) -> anyhow::Result<()> {
    let file_path = std::path::Path::new(file);
    anyhow::ensure!(file_path.exists(), "file not found: {file}");

    let ext = file_path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();

    match ext.as_str() {
        "luatos" => {
            let dir_path = std::path::Path::new(dir);
            std::fs::create_dir_all(dir_path)?;
            let result = luatos_project::archive::import_archive(file_path, dir_path)?;
            match format {
                OutputFormat::Text => {
                    println!("Imported archive: {file}");
                    println!("  Project: {}", result.project.project.name);
                    println!("  Chip:    {}", result.project.project.chip);
                    println!("  Files:   {}", result.files_extracted.len());
                    for f in &result.files_extracted {
                        println!("    {f}");
                    }
                    println!("  Output:  {}", result.output_dir.display());
                }
                OutputFormat::Json | OutputFormat::Jsonl => event::emit_result(
                    format,
                    "project.import",
                    "ok",
                    serde_json::json!({
                        "source": file,
                        "project": result.project,
                        "files": result.files_extracted,
                        "dir": result.output_dir.display().to_string(),
                    }),
                )?,
            }
        }
        "ini" => {
            let dir_path = std::path::Path::new(dir);
            let (project, lt_project) = luatos_project::import::import_luatools_ini(file_path)?;
            std::fs::create_dir_all(dir_path)?;
            project.save(dir_path)?;
            match format {
                OutputFormat::Text => {
                    println!("Imported LuaTools project: {}", project.project.name);
                    println!("  Chip:     {}", project.project.chip);
                    println!("  Scripts:  {} directories", lt_project.script_sections.len());
                    for section in &lt_project.script_sections {
                        println!("    {} ({} files)", section.dir_path, section.files.len());
                    }
                    if let Some(ref soc) = project.flash.soc_file {
                        println!("  SOC:      {soc}");
                    }
                    println!("  Config:   {dir}/luatos-project.toml");
                }
                OutputFormat::Json | OutputFormat::Jsonl => {
                    let sections: Vec<serde_json::Value> = lt_project
                        .script_sections
                        .iter()
                        .map(|s| serde_json::json!({ "dir": s.dir_path, "files": s.files }))
                        .collect();
                    event::emit_result(
                        format,
                        "project.import",
                        "ok",
                        serde_json::json!({
                            "project": project,
                            "source_sections": sections,
                        }),
                    )?;
                }
            }
        }
        other => {
            anyhow::bail!("unsupported file type '.{other}' — expected .ini (LuaTools) or .luatos (archive)");
        }
    }
    Ok(())
}

pub fn cmd_project_export(dir: &str, output: Option<&str>, format: &OutputFormat) -> anyhow::Result<()> {
    let dir_path = std::path::Path::new(dir);
    let project = luatos_project::Project::load(dir_path)?;

    // Default output: <project_name>.luatos in current directory
    let out_path_string: String = output.map(|s| s.to_string()).unwrap_or_else(|| {
        let safe_name = project.project.name.replace(|c: char| !c.is_alphanumeric() && c != '-' && c != '_', "_");
        format!("{safe_name}.luatos")
    });
    let out_path = std::path::Path::new(&out_path_string);

    let result = luatos_project::archive::export_project(dir_path, out_path)?;

    match format {
        OutputFormat::Text => {
            println!("Exported: {}", result.output.display());
            println!("  Project: {}", result.project_name);
            println!("  Chip:    {}", result.chip);
            println!("  Files:   {}", result.files_added.len());
            for f in &result.files_added {
                println!("    {f}");
            }
        }
        OutputFormat::Json | OutputFormat::Jsonl => event::emit_result(
            format,
            "project.export",
            "ok",
            serde_json::json!({
                "output": result.output.display().to_string(),
                "project": result.project_name,
                "chip": result.chip,
                "files": result.files_added,
            }),
        )?,
    }
    Ok(())
}

pub fn cmd_project_analyze(dir: &str, soc_override: Option<&str>, format: &OutputFormat) -> anyhow::Result<()> {
    use luatos_luadb::build::compile_lua_bytes;
    use luatos_luadb::{add_bk_crc, pack_luadb, LuadbEntry};
    use luatos_project::analyze::collect_project_files;
    use luatos_project::lua_deps::analyze_deps;

    let dir_path = std::path::Path::new(dir);
    let project = luatos_project::Project::load(dir_path)?;

    // ── 1. Collect all source files with raw sizes ──────────────────────────
    let files = collect_project_files(&project.build.script_dirs, &project.build.script_files, dir_path)?;

    // ── 2. Dependency graph ──────────────────────────────────────────────────
    let abs_dirs: Vec<String> = project.build.script_dirs.iter().map(|d| dir_path.join(d).to_string_lossy().into_owned()).collect();
    let abs_files: Vec<String> = project.build.script_files.iter().map(|f| dir_path.join(f).to_string_lossy().into_owned()).collect();
    let dep_graph = analyze_deps(&abs_dirs, &abs_files)?;

    // Files included in the build (respects ignore_deps)
    let included: std::collections::BTreeSet<&String> = if project.build.ignore_deps {
        files.keys().collect()
    } else {
        dep_graph.reachable.iter().collect()
    };

    // ── 3. Per-file syntax check + optional compile + size ──────────────────
    struct FileResult {
        filename: String,
        raw_size: usize,
        built_size: Option<usize>, // after compile (or same as raw for non-lua / no-luac)
        included: bool,
        syntax_error: Option<String>,
    }

    let strip = !project.build.luac_debug;
    let bitw = project.build.bitw;
    let use_luac = project.build.use_luac;

    let mut results: Vec<FileResult> = Vec::new();
    let mut syntax_errors: Vec<(String, String)> = Vec::new();
    let mut luadb_entries: Vec<LuadbEntry> = Vec::new();

    // Sort filenames for deterministic output (main.lua first)
    let mut sorted_names: Vec<&String> = files.keys().collect();
    sorted_names.sort_by(|a, b| {
        let a_main = a.as_str() == "main.lua" || a.as_str() == "main.luac";
        let b_main = b.as_str() == "main.lua" || b.as_str() == "main.luac";
        b_main.cmp(&a_main).then(a.cmp(b))
    });

    for name in &sorted_names {
        let pf = &files[*name];
        let is_included = included.contains(*name);

        if pf.is_lua && name.ends_with(".lua") {
            let source = fs::read(&pf.path)?;
            let chunk = format!("@{name}");
            match compile_lua_bytes(&source, &chunk, strip, bitw) {
                Ok(bytecode) => {
                    let built_size = if use_luac { bytecode.len() } else { pf.raw_size };
                    if is_included {
                        let entry_name = name.replace(".lua", ".luac");
                        let entry_data = if use_luac { bytecode } else { source };
                        luadb_entries.push(LuadbEntry {
                            filename: entry_name,
                            data: entry_data,
                        });
                    }
                    results.push(FileResult {
                        filename: (*name).clone(),
                        raw_size: pf.raw_size,
                        built_size: Some(built_size),
                        included: is_included,
                        syntax_error: None,
                    });
                }
                Err(e) => {
                    let msg = e.to_string();
                    syntax_errors.push(((*name).clone(), msg.clone()));
                    results.push(FileResult {
                        filename: (*name).clone(),
                        raw_size: pf.raw_size,
                        built_size: None,
                        included: is_included,
                        syntax_error: Some(msg),
                    });
                }
            }
        } else {
            // .luac or non-Lua resource file: no compilation
            let source = if is_included { fs::read(&pf.path).ok() } else { None };
            if let (true, Some(data)) = (is_included, source) {
                luadb_entries.push(LuadbEntry { filename: (*name).clone(), data });
            }
            results.push(FileResult {
                filename: (*name).clone(),
                raw_size: pf.raw_size,
                built_size: Some(pf.raw_size),
                included: is_included,
                syntax_error: None,
            });
        }
    }

    // ── 4. LuaDB image size ──────────────────────────────────────────────────
    // Resolve SOC file: CLI override → project config → None
    let soc_path_resolved: Option<std::path::PathBuf> = soc_override
        .map(std::path::PathBuf::from)
        .or_else(|| project.flash.soc_file.as_ref().map(|p| dir_path.join(p)));

    let soc_info = soc_path_resolved
        .as_deref()
        .filter(|p| p.exists())
        .map(|p| luatos_soc::read_soc_info(&p.to_string_lossy()))
        .transpose()?;

    let use_bkcrc = soc_info.as_ref().map(|i| i.use_bkcrc()).unwrap_or(false);

    let image_size: Option<usize> = if syntax_errors.is_empty() {
        let raw = pack_luadb(&luadb_entries)?;
        let image = if use_bkcrc { add_bk_crc(&raw) } else { raw };
        Some(image.len())
    } else {
        None // can't build a complete image with broken files
    };

    let partition_size: Option<usize> = soc_info.as_ref().map(|i| i.script_size());

    // ── 5. Output ────────────────────────────────────────────────────────────
    let unreachable_names: Vec<&String> = files.keys().filter(|n| !dep_graph.reachable.contains(*n)).collect();

    match format {
        OutputFormat::Text => {
            println!("Project analysis: {}", project.project.name);
            println!("  Chip: {}  |  bitw: {}  |  use_luac: {}", project.project.chip, bitw, use_luac);

            // — Syntax Check —
            println!("\n── Syntax Check ──────────────────────────────────────");
            let lua_files: Vec<&FileResult> = results.iter().filter(|r| r.filename.ends_with(".lua")).collect();
            if lua_files.is_empty() {
                println!("  (no .lua files)");
            } else {
                for r in &lua_files {
                    if let Some(ref err) = r.syntax_error {
                        println!("  ✗  {}", r.filename);
                        for line in err.lines() {
                            println!("       {line}");
                        }
                    } else {
                        println!("  ✓  {}", r.filename);
                    }
                }
                if syntax_errors.is_empty() {
                    println!("  All {} file(s) OK", lua_files.len());
                } else {
                    println!("  {} error(s) in {} file(s)", syntax_errors.len(), syntax_errors.len());
                }
            }

            // — Dependencies —
            println!("\n── Dependencies ──────────────────────────────────────");
            println!("  Total files:  {}", files.len());
            println!(
                "  Included:     {} ({}reachable from main.lua)",
                included.len(),
                if project.build.ignore_deps { "ignore_deps=true, all " } else { "" }
            );
            println!("  Excluded:     {}", unreachable_names.len());
            if !dep_graph.deps.is_empty() {
                println!("  Dep tree:");
                for (file, deps) in &dep_graph.deps {
                    let marker = if dep_graph.reachable.contains(file) { "✓" } else { "✗" };
                    if deps.is_empty() {
                        println!("    {marker} {file}");
                    } else {
                        println!("    {marker} {file}  →  {}", deps.join(", "));
                    }
                }
            }
            if !unreachable_names.is_empty() {
                println!("  Excluded (not reachable):");
                for n in &unreachable_names {
                    println!("    ✗ {n}");
                }
            }

            // — Space Usage —
            println!("\n── Space Usage ───────────────────────────────────────");
            let col_w = results.iter().map(|r| r.filename.len()).max().unwrap_or(8).max(8);
            println!("  {:<col_w$}  {:>10}  {:>10}  Status", "File", "Raw", "Built");
            println!("  {}  {}  {}  ------", "-".repeat(col_w), "-".repeat(10), "-".repeat(10));

            let mut total_raw = 0usize;
            let mut total_built = 0usize;
            for r in &results {
                let raw_str = fmt_bytes(r.raw_size);
                let built_str = r.built_size.map(fmt_bytes).unwrap_or_else(|| "  (error)".into());
                let status = match (r.included, r.syntax_error.is_some()) {
                    (_, true) => "✗ error",
                    (true, _) => "✓",
                    (false, _) => "— excluded",
                };
                println!("  {:<col_w$}  {:>10}  {:>10}  {status}", r.filename, raw_str, built_str);
                if r.included {
                    total_raw += r.raw_size;
                    total_built += r.built_size.unwrap_or(r.raw_size);
                }
            }
            println!("  {}  {}  {}  ------", "-".repeat(col_w), "-".repeat(10), "-".repeat(10));
            println!("  {:<col_w$}  {:>10}  {:>10}  (included files)", "TOTAL", fmt_bytes(total_raw), fmt_bytes(total_built));

            if let Some(img) = image_size {
                println!("  {:<col_w$}  {:>10}  {:>10}  (with LuaDB header)", "LuaDB image", "", fmt_bytes(img));
            }

            // — Partition Space —
            if let (Some(img), Some(part)) = (image_size, partition_size) {
                let soc_name = soc_path_resolved
                    .as_ref()
                    .and_then(|p| p.file_name())
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "soc".into());
                let used_pct = img as f64 / part as f64 * 100.0;
                let remaining = part.saturating_sub(img);
                println!("\n── Partition Space ({soc_name}) ─────────────────────");
                println!("  Partition:  {:>10}  ({} KB)", fmt_bytes(part), part / 1024);
                println!("  Image:      {:>10}  ({:.1}% used)", fmt_bytes(img), used_pct);
                println!("  Remaining:  {:>10}  ({:.1}% free)", fmt_bytes(remaining), 100.0 - used_pct);
                if img > part {
                    println!("  ⚠ IMAGE EXCEEDS PARTITION — reduce scripts or enable compression");
                }
            } else if partition_size.is_none() {
                println!("\n  Tip: pass --soc <path> to see partition space usage");
            }
        }

        OutputFormat::Json | OutputFormat::Jsonl => {
            let file_list: Vec<serde_json::Value> = results
                .iter()
                .map(|r| {
                    serde_json::json!({
                        "file": r.filename,
                        "raw_bytes": r.raw_size,
                        "built_bytes": r.built_size,
                        "included": r.included,
                        "syntax_error": r.syntax_error,
                    })
                })
                .collect();

            let deps_map: serde_json::Map<String, serde_json::Value> = dep_graph.deps.iter().map(|(k, v)| (k.clone(), serde_json::json!(v))).collect();

            event::emit_result(
                format,
                "project.analyze",
                "ok",
                serde_json::json!({
                    "project": project.project.name,
                    "chip": project.project.chip,
                    "syntax_errors": syntax_errors.iter().map(|(f,e)| serde_json::json!({"file":f,"error":e})).collect::<Vec<_>>(),
                    "files": file_list,
                    "dependencies": deps_map,
                    "reachable": dep_graph.reachable,
                    "unreachable": unreachable_names,
                    "external_modules": dep_graph.external_modules,
                    "image_size_bytes": image_size,
                    "partition_size_bytes": partition_size,
                    "remaining_bytes": match (image_size, partition_size) {
                        (Some(img), Some(part)) => serde_json::json!(part.saturating_sub(img)),
                        _ => serde_json::Value::Null,
                    },
                }),
            )?;
        }
    }

    // Exit with error code if there are syntax errors (useful for CI)
    if !syntax_errors.is_empty() {
        anyhow::bail!("{} syntax error(s) found", syntax_errors.len());
    }

    Ok(())
}

fn fmt_bytes(n: usize) -> String {
    if n >= 1024 * 1024 {
        format!("{:.1} MB", n as f64 / 1048576.0)
    } else if n >= 1024 {
        format!("{:.1} KB", n as f64 / 1024.0)
    } else {
        format!("{n} B")
    }
}

pub fn cmd_project_deps(dir: &str, show_reachable: bool, show_unreachable: bool, format: &OutputFormat) -> anyhow::Result<()> {
    let dir_path = std::path::Path::new(dir);
    let project = luatos_project::Project::load(dir_path)?;

    // Resolve script dirs relative to project dir
    let script_dirs: Vec<String> = project
        .build
        .script_dirs
        .iter()
        .map(|d| {
            let p = dir_path.join(d);
            p.to_string_lossy().into_owned()
        })
        .collect();
    let script_files: Vec<String> = project
        .build
        .script_files
        .iter()
        .map(|f| {
            let p = dir_path.join(f);
            p.to_string_lossy().into_owned()
        })
        .collect();

    let graph = luatos_project::lua_deps::analyze_deps(&script_dirs, &script_files)?;

    let all_files: Vec<&String> = graph.files.keys().collect();
    let unreachable: Vec<&String> = all_files.iter().filter(|f| !graph.reachable.contains(**f)).copied().collect();

    match format {
        OutputFormat::Text => {
            if show_reachable {
                println!("Reachable files ({}):", graph.reachable.len());
                for f in &graph.reachable {
                    println!("  ✓ {f}");
                }
            } else if show_unreachable {
                println!("Unreachable files ({}):", unreachable.len());
                for f in &unreachable {
                    println!("  ✗ {f}");
                }
            } else {
                // Show full dependency analysis
                println!("Dependency analysis for '{}'", project.project.name);
                println!("  Total files:      {}", graph.files.len());
                println!("  Reachable:        {}", graph.reachable.len());
                println!("  Unreachable:      {}", unreachable.len());
                println!("  External modules: {}", graph.external_modules.len());
                println!("  Ignore deps:      {}", project.build.ignore_deps);

                if !graph.deps.is_empty() {
                    println!("\nDependencies:");
                    for (file, deps) in &graph.deps {
                        if !deps.is_empty() {
                            let marker = if graph.reachable.contains(file) { "✓" } else { "✗" };
                            println!("  {marker} {file} → {}", deps.join(", "));
                        }
                    }
                }

                if !unreachable.is_empty() {
                    println!("\nUnreachable (can be excluded):");
                    for f in &unreachable {
                        println!("  ✗ {f}");
                    }
                }

                if !graph.external_modules.is_empty() {
                    println!("\nExternal/builtin modules:");
                    for m in &graph.external_modules {
                        println!("  • {m}");
                    }
                }
            }
        }
        OutputFormat::Json | OutputFormat::Jsonl => {
            let deps_map: serde_json::Map<String, serde_json::Value> = graph.deps.iter().map(|(k, v)| (k.clone(), serde_json::json!(v))).collect();
            event::emit_result(
                format,
                "project.deps",
                "ok",
                serde_json::json!({
                    "total_files": graph.files.len(),
                    "reachable": graph.reachable,
                    "unreachable": unreachable,
                    "external_modules": graph.external_modules,
                    "dependencies": deps_map,
                    "ignore_deps": project.build.ignore_deps,
                }),
            )?;
        }
    }
    Ok(())
}

/// 构建项目脚本镜像（读取 luatos-project.toml 配置，自动处理 soc_script 扩展库）
///
/// 等同于 `build filesystem`，但通过项目配置文件获取所有参数，并额外：
/// - 根据 `build.soc_script` 配置解析扩展库 `lib/` 目录，追加到源目录列表末尾
/// - 若未找到对应版本，打印提示后退出（不构建）
pub fn cmd_project_build(dir: &str, format: &crate::OutputFormat) -> anyhow::Result<()> {
    let dir_path = std::path::Path::new(dir);
    let project = luatos_project::Project::load(dir_path)?;

    // 解析 soc_script lib 目录
    let soc_lib = luatos_project::resolve_soc_script_lib_dir(dir_path, &project.build)?;

    // 构建源目录列表（项目 script_dirs + 可选的 soc_script lib）
    let mut src_dirs: Vec<String> = project.build.script_dirs.iter().map(|d| dir_path.join(d).to_string_lossy().into_owned()).collect();
    if let Some(lib) = soc_lib {
        src_dirs.push(lib.to_string_lossy().into_owned());
    }

    let output = dir_path.join(&project.build.output_dir).join("script.bin");
    let output_str = output.to_string_lossy().into_owned();

    crate::cmd_build::cmd_build_filesystem(
        &src_dirs,
        &output_str,
        project.build.use_luac,
        project.build.bitw,
        false, // bkcrc 由芯片类型决定，此处保守处理
        format,
    )
}
