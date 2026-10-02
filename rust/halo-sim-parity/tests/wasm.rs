//! The WebAssembly build of the simulation, executed, must give the same
//! bytes as the native build: the server runs one and the client the other,
//! and client prediction is only worth anything if they agree.
//!
//! The test builds `halo-sim-parity` for `wasm32-unknown-unknown` (with the
//! optimiser on, as it ships), loads the `.wasm` into a pure-Rust WebAssembly
//! interpreter with no imports at all (so the module cannot be calling any
//! host maths either), runs the scenario there and compares with the native
//! run. It needs no game data. The wasm target must be installed
//! (`rustup target add wasm32-unknown-unknown`); a missing target is an error,
//! not a skip.

use std::path::PathBuf;
use std::process::Command;

use halo_sim_parity::{event_counts, match_event_counts, run, run_match};
use wasmi::{Engine, Linker, Module, Store};

const TICKS: u32 = 10_000;

fn build_wasm() -> Vec<u8> {
    let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..");
    // a target directory of its own: this runs inside `cargo test`, which
    // holds the lock on the usual one
    let target_dir = workspace.join("target").join("wasm-parity");
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let status = Command::new(cargo)
        .current_dir(&workspace)
        .args(["build", "--release", "--locked", "--target", "wasm32-unknown-unknown", "-p", "halo-sim-parity"])
        .arg("--target-dir")
        .arg(&target_dir)
        .status()
        .expect("could not run cargo");
    assert!(status.success(), "building for wasm32-unknown-unknown failed (is the target installed?)");
    let path = target_dir.join("wasm32-unknown-unknown/release/halo_sim_parity.wasm");
    std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

struct Wasm {
    store: Store<()>,
    instance: wasmi::Instance,
}

impl Wasm {
    fn load(bytes: &[u8]) -> Wasm {
        let engine = Engine::default();
        let module = Module::new(&engine, bytes).expect("the module is valid WebAssembly");
        let mut store = Store::new(&engine, ());
        // no imports are defined, so instantiation fails if the module wants any
        let instance = Linker::<()>::new(&engine)
            .instantiate_and_start(&mut store, &module)
            .expect("the module instantiates with no imports");
        Wasm { store, instance }
    }

    fn run(&mut self, seed: u64, ticks: u32) -> Vec<u8> {
        self.call("parity_run", seed, ticks)
    }

    fn run_match(&mut self, seed: u64, ticks: u32) -> Vec<u8> {
        self.call("parity_match_run", seed, ticks)
    }

    fn call(&mut self, export: &str, seed: u64, ticks: u32) -> Vec<u8> {
        let run = self.instance.get_typed_func::<(u32, u32, u32), u32>(&self.store, export).unwrap();
        let len = run.call(&mut self.store, (seed as u32, (seed >> 32) as u32, ticks)).unwrap() as usize;
        let ptr = self.instance.get_typed_func::<(), u32>(&self.store, "parity_output").unwrap();
        let ptr = ptr.call(&mut self.store, ()).unwrap() as usize;
        let memory = self.instance.get_memory(&self.store, "memory").unwrap();
        memory.data(&self.store)[ptr..ptr + len].to_vec()
    }
}

#[test]
fn the_wasm_build_gives_byte_identical_state_to_the_native_build_over_10000_ticks() {
    let mut wasm = Wasm::load(&build_wasm());
    for seed in [1, 0xDEAD_BEEF_0BAD_F00D] {
        let native = run(seed, TICKS);
        let wasm_result = wasm.run(seed, TICKS);
        assert_eq!(wasm_result.len(), native.len());
        assert!(wasm_result == native, "seed {seed:#x}: the wasm and native results differ");

        // the scenario really exercised the step: it accepted and rejected every way
        let counts = event_counts(&native);
        assert!(counts.iter().all(|&c| c > 100), "scenario too thin: {counts:?}");
    }
}

/// The game's rules (spawning, waves, deaths, scores, the end) over a long
/// match: the starting locations are chosen with square roots and a random
/// source, the scores with integers, and all of it must agree.
#[test]
fn the_wasm_build_plays_a_match_byte_identically_to_the_native_build() {
    let mut wasm = Wasm::load(&build_wasm());
    for seed in [1, 2, 3, 0xDEAD_BEEF_0BAD_F00D] {
        let native = run_match(seed, 6_000);
        let wasm_result = wasm.run_match(seed, 6_000);
        assert_eq!(wasm_result.len(), native.len(), "seed {seed:#x}");
        assert!(wasm_result == native, "seed {seed:#x}: the wasm and native matches differ");

        // the match really played: every kind of event, bar none
        let counts = match_event_counts(&native);
        assert!(
            counts[..6].iter().all(|&c| c > 5) && counts[6] >= 1,
            "seed {seed:#x}: the match was too thin: {counts:?}"
        );
    }
}

#[test]
fn different_seeds_give_different_results() {
    // guards the comparison above against comparing two empty or constant results
    assert_ne!(run(1, 300), run(2, 300));
    assert_ne!(run_match(1, 600), run_match(3, 600));
}
