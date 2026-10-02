//! Prints a summary of a map file: `cargo run --example dump -- path/to/bloodgulch.map`.

fn main() {
    let path = std::env::args().nth(1).expect("usage: dump <map file>");
    let m = halo_map::HaloMap::from_path(path).unwrap();
    let mut starts = std::collections::BTreeMap::new();
    for s in &m.player_starts {
        *starts.entry((s.team_index, s.game_types)).or_insert(0) += 1;
    }
    println!("{} ({}), bsp {}", m.header.name, m.scenario_name, m.structure_bsp_name);
    println!("bounds {:?}", m.world_bounds);
    println!("starts by (team, game types): {starts:?}");
    println!("movement {:?}", m.movement);
    println!("{} netgame flags, first: {:?}", m.netgame_flags.len(), m.netgame_flags.first());
    println!("equipment: {:?}", &m.netgame_equipment[..m.netgame_equipment.len().min(3)]);
    println!("vehicles: {:?}", &m.vehicles[..m.vehicles.len().min(3)]);
}
