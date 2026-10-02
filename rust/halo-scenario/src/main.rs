//! `halo-scenario <scenario.scn> --maps <folder with the .map files> [--out <trace file>]`
//!
//! Plays a comparison scenario in the Rust simulation and writes its trace
//! (to the file, or standard output). Exit status: 0, 1 for a scenario or map
//! that cannot be played, 2 for bad arguments.

use std::process::ExitCode;

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let (mut scenario, mut maps, mut out) = (None, None, None);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--maps" => maps = args.next(),
            "--out" => out = args.next(),
            other if other.starts_with("--") => {
                eprintln!("unknown option {other}");
                return ExitCode::from(2);
            }
            _ => scenario = Some(arg),
        }
    }
    let (Some(scenario), Some(maps)) = (scenario, maps.or_else(|| std::env::var("HALO_MAP_DIR").ok())) else {
        eprintln!("usage: halo-scenario <scenario.scn> --maps <folder of .map files> [--out <trace file>]");
        return ExitCode::from(2);
    };
    match play(&scenario, &maps) {
        Ok(text) => match out {
            Some(path) => match std::fs::write(&path, text) {
                Ok(()) => {
                    eprintln!("wrote {path}");
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("cannot write {path}: {e}");
                    ExitCode::FAILURE
                }
            },
            None => {
                print!("{text}");
                ExitCode::SUCCESS
            }
        },
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}

fn play(scenario_path: &str, maps: &str) -> Result<String, String> {
    let text = std::fs::read_to_string(scenario_path).map_err(|e| format!("cannot read {scenario_path}: {e}"))?;
    let scenario = halo_scenario::parse(&text).map_err(|e| format!("{scenario_path}: {e}"))?;
    let map_path = std::path::Path::new(maps).join(format!("{}.map", scenario.map));
    let map = halo_map::HaloMap::from_path(&map_path).map_err(|e| format!("{}: {e}", map_path.display()))?;
    halo_scenario::trace(&scenario, &map.into())
}
