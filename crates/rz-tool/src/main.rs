use std::env;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use rz_assets::{
    build_project, describe_error_chain, extract_project, BuildOptions, ExtractOptions, WrapMode,
};
use rz_assets::eboot;

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
        "extract-sc-alloc" => extract_sc_alloc_command(&arguments[1..]),
        "extract-lt-alloc" => extract_lt_alloc_command(&arguments[1..]),
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
    let mut positional = Vec::<String>::new();
    let mut charset_map = None::<PathBuf>;
    let mut allocation_map = None::<PathBuf>;
    let mut raw_only = false;
    let mut debug_script_ir = false;
    let mut use_stock_charset = false;
    let mut use_stock_allocation = false;
    let mut index = 0usize;
    while index < arguments.len() {
        match arguments[index].as_str() {
            "--raw-only" => raw_only = true,
            "--debug-script-ir" => debug_script_ir = true,
            "--use-stock-charset" => use_stock_charset = true,
            "--use-stock-allocation" => use_stock_allocation = true,
            "--charset-map" => {
                index += 1;
                let value = arguments.get(index).ok_or("--charset-map requires a path")?;
                if charset_map.replace(PathBuf::from(value)).is_some() {
                    return Err("--charset-map was provided more than once".into());
                }
            }
            "--allocation-map" => {
                index += 1;
                let value = arguments.get(index).ok_or("--allocation-map requires a path")?;
                if allocation_map.replace(PathBuf::from(value)).is_some() {
                    return Err("--allocation-map was provided more than once".into());
                }
            }
            value if value.starts_with('-') => return Err(format!("unknown option {value}").into()),
            value => positional.push(value.to_owned()),
        }
        index += 1;
    }
    if positional.len() != 2 {
        return Err("extract requires <input.cpk|lt.bin|pr.bin> <project-directory>".into());
    }
    let options = ExtractOptions {
        raw_only,
        debug_script_ir,
        charset_map,
        allocation_map,
        use_stock_charset,
        use_stock_allocation,
    };
    let report = extract_project(Path::new(&positional[0]), Path::new(&positional[1]), options)?;
    println!(
        "extracted {} entries as {} project",
        report.files,
        if report.editable { "editable" } else { "raw-only" }
    );
    Ok(())
}

fn extract_sc_alloc_command(arguments: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    if arguments.len() != 2 {
        return Err("extract-sc-alloc requires <eboot.bin.elf> <sc-allocation.json>".into());
    }
    let input = Path::new(&arguments[0]);
    let output = Path::new(&arguments[1]);
    if output.exists() {
        return Err(format!("output already exists: {}", output.display()).into());
    }
    let map = eboot::extract_sc_allocation_map(input)?;
    std::fs::write(output, serde_json::to_vec_pretty(&map)?)?;
    println!("wrote SC allocation map for {} entries", map.entries.len());
    Ok(())
}

fn extract_lt_alloc_command(arguments: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    if arguments.len() != 2 {
        return Err("extract-lt-alloc requires <eboot.bin.elf> <lt-allocation.json>".into());
    }
    let input = Path::new(&arguments[0]);
    let output = Path::new(&arguments[1]);
    if output.exists() {
        return Err(format!("output already exists: {}", output.display()).into());
    }
    let map = eboot::extract_lt_allocation_map(input)?;
    std::fs::write(output, serde_json::to_vec_pretty(&map)?)?;
    println!(
        "wrote LT allocation map: glyph_count={:#x}, allocation={:#x}, sectors={:#x}",
        map.glyph_count, map.allocation_size, map.sector_count
    );
    Ok(())
}

fn build_command(arguments: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let mut positional = Vec::<String>::new();
    let mut eboot_in = None::<PathBuf>;
    let mut eboot_out = None::<PathBuf>;
    let mut force = false;
    let mut charset_map = None::<PathBuf>;
    let mut wrap_width_px = None::<u32>;
    let mut wrap_width_table = None::<PathBuf>;
    let mut wrap_rows = None::<u32>;
    let mut wrap_mode = None::<WrapMode>;
    let mut index = 0usize;
    while index < arguments.len() {
        match arguments[index].as_str() {
            "-f" | "--force" => {
                if force {
                    return Err("-f/--force was provided more than once".into());
                }
                force = true;
            }
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
            "--wrap-width-px" => {
                index += 1;
                let value = arguments.get(index).ok_or("--wrap-width-px requires a value")?;
                if wrap_width_px.replace(value.parse::<u32>()?).is_some() {
                    return Err("--wrap-width-px was provided more than once".into());
                }
            }
            "--wrap-width-table" => {
                index += 1;
                let value = arguments.get(index).ok_or("--wrap-width-table requires a path")?;
                if wrap_width_table.replace(PathBuf::from(value)).is_some() {
                    return Err("--wrap-width-table was provided more than once".into());
                }
            }
            "--wrap-mode" => {
                index += 1;
                let value = arguments.get(index).ok_or("--wrap-mode requires word or legacy")?;
                let parsed = match value.as_str() {
                    "word" => WrapMode::Word,
                    "legacy" => WrapMode::Legacy,
                    other => return Err(format!("--wrap-mode must be word or legacy, got {other:?}").into()),
                };
                if wrap_mode.replace(parsed).is_some() {
                    return Err("--wrap-mode was provided more than once".into());
                }
            }
            "--wrap-rows" => {
                index += 1;
                let value = arguments.get(index).ok_or("--wrap-rows requires a value")?;
                if wrap_rows.replace(value.parse::<u32>()?).is_some() {
                    return Err("--wrap-rows was provided more than once".into());
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
    if wrap_width_px.is_some() && wrap_width_table.is_none() {
        return Err("--wrap-width-px requires --wrap-width-table <font.cnf>".into());
    }
    if wrap_mode.is_some() && wrap_width_table.is_none() {
        return Err("--wrap-mode requires --wrap-width-table <font.cnf>".into());
    }
    if wrap_rows.is_some() && wrap_width_table.is_none() {
        return Err("--wrap-rows requires --wrap-width-table <font.cnf>".into());
    }
    let options = BuildOptions {
        eboot_in,
        eboot_out,
        force,
        charset_map,
        wrap_width_px,
        wrap_width_table,
        wrap_rows,
        wrap_mode,
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
        if patch.forced_runtime_hash_mismatch {
            eprintln!("warning: -f/--force bypassed VWF_RUNTIME_HASH_RANGE_SHA256 mismatch for SC EBOOT patching");
        }
    }
    if let Some(patch) = report.lt_eboot_patch {
        println!(
            "patched eboot: LT glyph_count {:#x}, allocation {:#x}, sectors {:#x}, changed={}",
            patch.glyph_count, patch.allocation_size, patch.sector_count, patch.changed
        );
        if patch.forced_runtime_hash_mismatch {
            eprintln!("warning: -f/--force bypassed VWF_RUNTIME_HASH_RANGE_SHA256 mismatch for LT EBOOT patching");
        }
    }
    Ok(())
}


fn print_usage() {
    eprintln!(
        r#"rz-tool

Usage:
  rz-tool extract <input.cpk|lt.bin> <project-directory>
  rz-tool extract-sc-alloc <patched_eboot.elf> <sc-allocation.json>
  rz-tool extract-lt-alloc <patched_eboot.elf> <lt-allocation.json>
  rz-tool extract <sc.cpk> <project-directory> [--charset-map <font.tbl|charset.json>] [--allocation-map <sc-allocation.json>] [--debug-script-ir]
  rz-tool extract <lt.bin> <project-directory> [--allocation-map <lt-allocation.json>]
  rz-tool extract <input.cpk|lt.bin|pr.bin> <project-directory> --raw-only
  rz-tool build <project-directory> <output.cpk|pr.bin>
  rz-tool build <lt-project> <lt.bin> --eboot-in <vwf-patched-eboot.elf> --eboot-out <patched_eboot.elf> [-f]
  rz-tool build <sc-project> <output.cpk> [--charset-map <charset.json>] [--wrap-width-table <font.cnf|json>] [--wrap-width-px <px>] [--wrap-mode <word|legacy>] [--wrap-rows <n>]
  rz-tool build <sc-project> <output.cpk> --eboot-in <vwf-patched-eboot.bin.elf> --eboot-out <patched.bin.elf> [-f] [--charset-map <font.tbl|charset.json>] [--wrap-width-table <font.cnf|json>] [--wrap-width-px <px>] [--wrap-mode <word|legacy>] [--wrap-rows <n>]

Editable mode understands the engine image-package grammar used by
addpt/bk/bsf/pt, compiled scene-script payloads in sc.cpk, and the lt.bin 4-bpp
glyph bank. For sc.cpk, scenario-dialogue.json is the sole user-editable text
document; dialogue markers are the user-visible dialogues and each marker uses
one complete editable `text` field. Wrapping is performed silently during build.
`--wrap-width-table` takes the same patcher font.cnf width config (or compiled
JSON) used for the VWF ELF width table. When `--charset-map` or
`--wrap-width-table` is used together with `--eboot-in/--eboot-out`, the
input ELF must already be patched by the standalone VWF patcher and must match
the known VWF runtime SHA-256 over VA range 0x81000000..0x81100000. `-f`/`--force`
may bypass only that hash mismatch when intentionally chaining EBOOT allocation updates.
SC builds patch SC allocation/metadata plus runtime buffer size; LT builds patch LT
allocation size/sector count. The standalone VWF patcher owns LT runtime glyph limits.
`--wrap-width-px`
overrides the default 528px physical row limit; when omitted, rz-tool uses
528px per row. A runtime dialogue screen holds three physical rows by default
(`--wrap-rows 3`). Word mode wraps before the next word would exceed the row
limit, groups rows into three-row screens, and emits continuation dialogue
markers for overflow screens. This avoids the engine overwrite mode observed
when extra rows were encoded as additional FFFE pages under the same marker.
Pass `--wrap-mode legacy` to use the old glyph-by-glyph breaker with the same
three-row screen grouping. The 528px row value is derived from runtime
calibration: 528px was observed safe, while 550px clipped the final 0> marker.
Routing and row-joiner/continuation metadata are stored inside
.rz-internal/sc-build-state.json.gz. Rebuild state is stored in that same
compressed bundle; normal extraction does not emit scenario-routing.json or
scenario-dialogue.meta.json.
When SC wrapping generates continuation markers or trims semantic row-boundary
spaces, build also writes `<output>.rz-dialogue-meta.json` beside the rebuilt
archive. Keep that companion with the SC file. A later extract automatically
loads it, verifies its archive SHA-256 and generated `FFFB FF68` marker chain,
then folds continuation markers back into one editable text field. The companion
contains only marker relationships and row joiners; it contains no dialogue,
speaker, row, width-profile, pixel-metric, or debug data.
Known stock sc.cpk hashes use builtin charset/allocation and export metadata.
Unknown or modified sc.cpk must be extracted with --charset-map and --allocation-map,
unless explicit --use-stock-charset/--use-stock-allocation overrides are supplied.
Pass --debug-script-ir only when human-readable per-entry IR/state copies are
needed under debug/scenario-ir.
Extracted image CPK assets are written into one flat project directory with
stable entry/package prefixes such as 00000-000.png. Editable projects contain
no *skeleton.bin files; image package fields whose producer semantics remain
unproven are preserved explicitly in package.json. Image edits are encoded in
the original GPU layout: unchanged BC blocks and GZIP chunks remain
byte-identical, changed BC blocks are source-seeded and verified after decode,
and P4/P8 palettes are rebuilt automatically.
pr.bin is accepted only with --raw-only because the engine uses
multiple incompatible slot grammars that are not all encoded yet. Audio/video
is rejected. Raw bytes are written only with explicit --raw-only."#
    );
}
