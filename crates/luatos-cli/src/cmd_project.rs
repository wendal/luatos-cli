use crate::OutputFormat;

pub fn cmd_project_new(
    dir: &str,
    name: &str,
    chip: &str,
    format: &OutputFormat,
) -> anyhow::Result<()> {
    let dir_path = std::path::Path::new(dir);
    luatos_project::scaffold_project(dir_path, name, chip)?;

    match format {
        OutputFormat::Text => {
            println!("Created project '{name}' for chip '{chip}' in {dir}/");
            println!("  Config: {dir}/luatos-project.toml");
            println!("  Script: {dir}/lua/main.lua");
            println!("\nNext steps:");
            println!("  cd {dir}");
            println!("  luatos-cli build filesystem --src lua/ --output build/script.bin");
        }
        OutputFormat::Json => {
            let json = serde_json::json!({
                "status": "ok",
                "command": "project.new",
                "data": { "name": name, "chip": chip, "dir": dir },
            });
            println!("{}", serde_json::to_string_pretty(&json)?);
        }
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
            if let Some(ref soc) = project.flash.soc_file {
                println!("  SOC:         {soc}");
            }
            if let Some(ref port) = project.flash.port {
                println!("  Port:        {port}");
            }
        }
        OutputFormat::Json => {
            let json = serde_json::json!({
                "status": "ok",
                "command": "project.info",
                "data": project,
            });
            println!("{}", serde_json::to_string_pretty(&json)?);
        }
    }
    Ok(())
}

pub fn cmd_project_config(
    dir: &str,
    key: Option<&str>,
    value: Option<&str>,
    format: &OutputFormat,
) -> anyhow::Result<()> {
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
                OutputFormat::Json => {
                    let json = serde_json::json!({
                        "status": "ok",
                        "command": "project.config",
                        "data": project,
                    });
                    println!("{}", serde_json::to_string_pretty(&json)?);
                }
            }
        }
        (Some(k), None) => {
            // Key only: get value
            let val = get_config_value(&project, k)?;
            match format {
                OutputFormat::Text => println!("{val}"),
                OutputFormat::Json => {
                    let json = serde_json::json!({
                        "status": "ok",
                        "command": "project.config",
                        "data": { "key": k, "value": val },
                    });
                    println!("{}", serde_json::to_string_pretty(&json)?);
                }
            }
        }
        (Some(k), Some(v)) => {
            // Key + value: set
            set_config_value(&mut project, k, v)?;
            project.save(dir_path)?;
            match format {
                OutputFormat::Text => println!("Set {k} = {v}"),
                OutputFormat::Json => {
                    let json = serde_json::json!({
                        "status": "ok",
                        "command": "project.config",
                        "data": { "key": k, "value": v },
                    });
                    println!("{}", serde_json::to_string_pretty(&json)?);
                }
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
        "flash.baud" => project
            .flash
            .baud
            .map(|b| b.to_string())
            .unwrap_or_default(),
        _ => anyhow::bail!("Unknown config key: {key}"),
    })
}

fn set_config_value(
    project: &mut luatos_project::Project,
    key: &str,
    value: &str,
) -> anyhow::Result<()> {
    match key {
        "project.name" => project.project.name = value.to_string(),
        "project.chip" => project.project.chip = value.to_string(),
        "project.version" => project.project.version = value.to_string(),
        "project.description" => project.project.description = Some(value.to_string()),
        "build.script_dir" | "build.script_dirs" => {
            project.build.script_dirs = value.split(',').map(|s| s.trim().to_string()).collect();
        }
        "build.script_files" => {
            project.build.script_files =
                value.split(',').map(|s| s.trim().to_string()).collect();
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

pub fn cmd_project_import(ini: &str, dir: &str, format: &OutputFormat) -> anyhow::Result<()> {
    let ini_path = std::path::Path::new(ini);
    anyhow::ensure!(ini_path.exists(), "INI file not found: {ini}");

    let (project, lt_project) = luatos_project::import::import_luatools_ini(ini_path)?;

    let dir_path = std::path::Path::new(dir);
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
            println!("  Config:   {}/luatos-project.toml", dir);
        }
        OutputFormat::Json => {
            let sections: Vec<serde_json::Value> = lt_project
                .script_sections
                .iter()
                .map(|s| {
                    serde_json::json!({
                        "dir": s.dir_path,
                        "files": s.files,
                    })
                })
                .collect();
            let json = serde_json::json!({
                "status": "ok",
                "command": "project.import",
                "data": {
                    "project": project,
                    "source_sections": sections,
                },
            });
            println!("{}", serde_json::to_string_pretty(&json)?);
        }
    }
    Ok(())
}

pub fn cmd_project_deps(
    dir: &str,
    show_reachable: bool,
    show_unreachable: bool,
    format: &OutputFormat,
) -> anyhow::Result<()> {
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
    let unreachable: Vec<&String> = all_files
        .iter()
        .filter(|f| !graph.reachable.contains(**f))
        .copied()
        .collect();

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
                println!(
                    "  Total files:      {}",
                    graph.files.len()
                );
                println!("  Reachable:        {}", graph.reachable.len());
                println!("  Unreachable:      {}", unreachable.len());
                println!("  External modules: {}", graph.external_modules.len());
                println!("  Ignore deps:      {}", project.build.ignore_deps);

                if !graph.deps.is_empty() {
                    println!("\nDependencies:");
                    for (file, deps) in &graph.deps {
                        if !deps.is_empty() {
                            let marker = if graph.reachable.contains(file) {
                                "✓"
                            } else {
                                "✗"
                            };
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
        OutputFormat::Json => {
            let deps_map: serde_json::Map<String, serde_json::Value> = graph
                .deps
                .iter()
                .map(|(k, v)| (k.clone(), serde_json::json!(v)))
                .collect();
            let json = serde_json::json!({
                "status": "ok",
                "command": "project.deps",
                "data": {
                    "total_files": graph.files.len(),
                    "reachable": graph.reachable,
                    "unreachable": unreachable,
                    "external_modules": graph.external_modules,
                    "dependencies": deps_map,
                    "ignore_deps": project.build.ignore_deps,
                },
            });
            println!("{}", serde_json::to_string_pretty(&json)?);
        }
    }
    Ok(())
}
