use std::env;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use rz_assets::{
    build_project, describe_error_chain, extract_project, BuildOptions, ExtractOptions,
};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {}", describe_error_chain(error.as_ref()));
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let arguments = env::args().skip(1).collect::<Vec<_>>();
    let Some(command) = arguments.first().map(String::as_str) else {
        print_usage();
        return Err("missing command".into());
    };
    match command {
        "extract" => extract_command(&arguments[1..]),
        "build" => build_command(&arguments[1..]),
        "help" | "--help" | "-h" => {
            print_usage();
            Ok(())
        }
        other => {
            print_usage();
            Err(format!("unknown command {other:?}").into())
        }
    }
}

fn extract_command(arguments: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let positional = arguments
        .iter()
        .filter(|argument| !argument.starts_with('-'))
        .collect::<Vec<_>>();
    if positional.len() != 2 {
        return Err("extract requires <input.cpk|lt.bin|pr.bin> <project-directory>".into());
    }
    reject_unknown_flags(arguments, &["--raw-only"])?;
    let options = ExtractOptions {
        raw_only: arguments.iter().any(|argument| argument == "--raw-only"),
    };
    let report = extract_project(Path::new(positional[0]), Path::new(positional[1]), options)?;
    println!(
        "extracted {} entries as {} project",
        report.files,
        if report.editable { "editable" } else { "raw-only" }
    );
    Ok(())
}

fn build_command(arguments: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let mut positional = Vec::<String>::new();
    let mut eboot_in = None::<PathBuf>;
    let mut eboot_out = None::<PathBuf>;
    let mut charset_map = None::<PathBuf>;
    let mut index = 0usize;
    while index < arguments.len() {
        match arguments[index].as_str() {
            "--eboot-in" => {
                index += 1;
                let value = arguments.get(index).ok_or("--eboot-in requires a path")?;
                if eboot_in.replace(PathBuf::from(value)).is_some() {
                    return Err("--eboot-in was provided more than once".into());
                }
            }
            "--eboot-out" => {
                index += 1;
                let value = arguments.get(index).ok_or("--eboot-out requires a path")?;
                if eboot_out.replace(PathBuf::from(value)).is_some() {
                    return Err("--eboot-out was provided more than once".into());
                }
            }
            "--charset-map" => {
                index += 1;
                let value = arguments.get(index).ok_or("--charset-map requires a path")?;
                if charset_map.replace(PathBuf::from(value)).is_some() {
                    return Err("--charset-map was provided more than once".into());
                }
            }
            value if value.starts_with('-') => {
                return Err(format!("unknown option {value}").into());
            }
            value => positional.push(value.to_owned()),
        }
        index += 1;
    }
    if positional.len() != 2 {
        return Err("build requires <project-directory> <output.cpk|lt.bin|pr.bin>".into());
    }
    let options = BuildOptions {
        eboot_in,
        eboot_out,
        charset_map,
    };
    let report = build_project(Path::new(&positional[0]), Path::new(&positional[1]), options)?;
    println!(
        "built {} logical assets, output size {:#x} bytes",
        report.files, report.output_size
    );
    if let Some(patch) = report.eboot_patch {
        println!(
            "patched eboot: SC buffer {:#x}, total SC sectors {}, changed={}",
            patch.script_buffer_size, patch.total_sectors, patch.changed
        );
    }
    Ok(())
}

fn reject_unknown_flags(arguments: &[String], accepted: &[&str]) -> Result<(), Box<dyn std::error::Error>> {
    for argument in arguments.iter().filter(|argument| argument.starts_with('-')) {
        if !accepted.contains(&argument.as_str()) {
            return Err(format!("unknown option {argument}").into());
        }
    }
    Ok(())
}

fn print_usage() {
    eprintln!(
        r#"rz-tool

Usage:
  rz-tool extract <input.cpk|lt.bin> <project-directory>
  rz-tool extract <input.cpk|lt.bin|pr.bin> <project-directory> --raw-only
  rz-tool build <project-directory> <output.cpk|lt.bin|pr.bin>
  rz-tool build <sc-project> <output.cpk> [--charset-map <charset.json>]
  rz-tool build <sc-project> <output.cpk> --eboot-in <eboot.bin.elf> --eboot-out <patched.bin.elf> [--charset-map <charset.json>]

Editable mode understands the engine image-package grammar used by
addpt/bk/bsf/pt, compiled scene-script payloads in sc.cpk, and the lt.bin 4-bpp
glyph bank. Every SC entry is exactly two JSON files: an editable relocatable
source IR (`NNNNN.script.json`) and machine-managed rebuild metadata
(`NNNNN.script-meta.json`). Extracted CPK assets are written into one
flat project directory with stable entry/package prefixes such as
00000-000.png. Editable projects contain no *skeleton.bin files; image package
fields whose producer semantics remain unproven are preserved explicitly in
package.json. Image edits are encoded in the original GPU layout: unchanged BC blocks and
GZIP chunks remain byte-identical, changed BC blocks are source-seeded and
verified after decode, and P4/P8 palettes are rebuilt automatically.
pr.bin is accepted only with --raw-only because the engine uses
multiple incompatible slot grammars that are not all encoded yet. Audio/video
is rejected. Raw bytes are written only with explicit --raw-only."#
    );
}
