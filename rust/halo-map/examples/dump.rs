//! Prints a summary of a map file: `cargo run --example dump -- path/to/bloodgulch.map`.

fn c_name(m: &halo_map::HaloMap, tag: u16) -> String {
    m.combat.weapon(tag).map_or_else(|| format!("tag #{tag} (not a weapon)"), |w| w.name.clone())
}

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
    let it = &m.items;
    println!("player reach {:?}", it.player);
    for d in &it.defs {
        println!("item {:?}", d);
    }
    for p in &it.placements {
        let names: Vec<String> = p
            .permutations
            .iter()
            .map(|(w, t)| format!("{w} x {}", it.def(*t).map_or("?", |d| d.name.as_str())))
            .collect();
        println!(
            "placement at {:?} flags {} types {:?} spawn {}s collection {}s -> {names:?}",
            p.position, p.flags, p.game_types, p.spawn_time, p.collection_spawn_time
        );
    }
    let c = &m.combat;
    println!("resistance {:?}", c.resistance);
    for s in &c.starting_equipment {
        let names: Vec<Vec<String>> = s
            .collections
            .iter()
            .map(|c| c.iter().map(|(w, t)| format!("{w} x {}", c_name(&m, *t))).collect())
            .collect();
        println!("starting equipment {:?} flags {} -> {names:?}", s.game_types, s.flags);
    }
    for w in &c.weapons {
        println!(
            "weapon {} #{}: type {} flags {:#x}, heat {}/{}/{} loss {}, {} magazines, {} triggers",
            w.name,
            w.tag_index,
            w.weapon_type,
            w.flags,
            w.heat_recovery_threshold,
            w.heat_overheated_threshold,
            w.heat_detonation_threshold,
            w.heat_loss_per_second,
            w.magazines.len(),
            w.triggers.len()
        );
        for (i, mag) in w.magazines.iter().enumerate() {
            println!("   magazine {i}: {mag:?}");
        }
        for (i, t) in w.triggers.iter().enumerate() {
            println!(
                "   trigger {i}: flags {:#x} rof {} -> {} accel {} decel {} mag {} per shot {} min {} proj/shot {} \
                 charge {} heat {}",
                t.flags,
                t.initial_rate_of_fire,
                t.final_rate_of_fire,
                t.rate_of_fire_acceleration,
                t.rate_of_fire_deceleration,
                t.magazine_index,
                t.rounds_per_shot,
                t.minimum_rounds_loaded_per_shot,
                t.projectiles_per_shot,
                t.charging_time,
                t.heat_generated_per_round
            );
            if let Some(p) = &t.projectile {
                println!(
                    "      projectile v {} -> {} range {} timer {}..{} damage {:?}",
                    p.initial_velocity,
                    p.final_velocity,
                    p.maximum_range,
                    p.timer_lower_bound,
                    p.timer_upper_bound,
                    p.impact_damage.map(|d| (d.flags, d.category, d.minimum, d.lower, d.upper))
                );
                if let Some(d) = &p.impact_damage {
                    println!("      material modifiers {:?}", d.material_modifiers);
                }
            }
        }
    }
}
