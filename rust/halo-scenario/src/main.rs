//! `halo-scenario <scenario.scn> --maps <folder with the .map files> [--out <trace file>] [--hits <engine trace>]`
//!
//! Plays a comparison scenario in the Rust simulation and writes its trace
//! (to the file, or standard output). With `--hits`, the hits that land on the
//! target are those the C engine's trace of the same scenario saw (the
//! engine's shot goes where its aim and the frame it is fired on put it: what
//! it hits is the client's to decide), not every shot. Exit status: 0, 1 for a
//! scenario or map that cannot be played, 2 for bad arguments.

use std::process::ExitCode;

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let (mut scenario, mut maps, mut out, mut hits) = (None, None, None, None);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--maps" => maps = args.next(),
            "--out" => out = args.next(),
            "--hits" => hits = args.next(),
            other if other.starts_with("--") => {
                eprintln!("unknown option {other}");
                return ExitCode::from(2);
            }
            _ => scenario = Some(arg),
        }
    }
    let (Some(scenario), Some(maps)) = (scenario, maps.or_else(|| std::env::var("HALO_MAP_DIR").ok())) else {
        eprintln!(
            "usage: halo-scenario <scenario.scn> --maps <folder of .map files> [--out <trace file>] \
             [--hits <engine trace>]"
        );
        return ExitCode::from(2);
    };
    match play(&scenario, &maps, hits.as_deref()) {
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

fn play(scenario_path: &str, maps: &str, hits: Option<&str>) -> Result<String, String> {
    let text = std::fs::read_to_string(scenario_path).map_err(|e| format!("cannot read {scenario_path}: {e}"))?;
    let scenario = halo_scenario::parse(&text).map_err(|e| format!("{scenario_path}: {e}"))?;
    let map_path = std::path::Path::new(maps).join(format!("{}.map", scenario.map));
    let map = halo_map::HaloMap::from_path(&map_path).map_err(|e| format!("{}: {e}", map_path.display()))?;
    let observed = match hits {
        Some(path) => {
            let trace = std::fs::read_to_string(path).map_err(|e| format!("cannot read {path}: {e}"))?;
            Some(halo_scenario::hits_of_trace(&trace).map_err(|e| format!("{path}: {e}"))?)
        }
        None => None,
    };
    halo_scenario::trace_with_hits(&scenario, &map.into(), observed.as_deref())
}
