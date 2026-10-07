//! Tests against the developer's own Xbox map files, found through the
//! `HALO_MAP_DIR` environment variable. Without it they pass without testing.

mod common;

use common::{brute_force_ground, map_dir, map_path, MAPS};
use halo_map::{flag_type, HaloMap};

fn load_all() -> Option<Vec<(&'static str, HaloMap)>> {
    let dir = map_dir()?;
    Some(
        MAPS.iter()
            .map(|&name| {
                let path = map_path(&dir, name);
                let map = HaloMap::from_path(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
                (name, map)
            })
            .collect(),
    )
}

#[test]
fn all_13_maps_load() {
    let Some(maps) = load_all() else { return };
    for (name, map) in &maps {
        assert_eq!(&map.header.name, name);
        assert!(!map.player_starts.is_empty(), "{name}: no player starts");
        assert!(!map.collision.bsp3d_nodes.is_empty(), "{name}: no collision BSP");
        let b = map.world_bounds;
        assert!(b[0] < b[1] && b[2] < b[3] && b[4] < b[5], "{name}: empty world bounds {b:?}");
    }
}

#[test]
fn a_map_loads_the_same_compressed_or_not() {
    let Some(dir) = map_dir() else { return };
    let file = std::fs::read(map_path(&dir, "carousel")).unwrap();
    let compressed = HaloMap::from_bytes(&file).unwrap();
    assert!(compressed.header.compressed, "the retail file is expected to be compressed");

    // inflate it by hand, as a map already unpacked on disk would be
    let mut image = file[..0x800].to_vec();
    std::io::Read::read_to_end(&mut flate2::read::ZlibDecoder::new(&file[0x800..]), &mut image).unwrap();
    let plain = HaloMap::from_bytes(&image).unwrap();
    assert!(!plain.header.compressed);

    let mut expected = compressed.clone();
    expected.header.compressed = false;
    assert_eq!(plain, expected);
}

#[test]
fn every_collision_index_is_in_range_on_every_map() {
    let Some(maps) = load_all() else { return };
    for (name, map) in &maps {
        let checks = map.collision.check_indices(map.collision_material_count);
        for c in &checks {
            assert_eq!(c.bad, 0, "{name}: {} indices out of range in {}", c.bad, c.field);
        }
        // the check looked at every block, and found something to check in the main ones
        assert_eq!(checks.len(), 18);
        for c in checks.iter().filter(|c| !c.field.contains("bsp2d node")) {
            assert!(c.checked > 0, "{name}: nothing to check for {}", c.field);
        }
    }
}

#[test]
fn a_ray_dropped_from_just_above_each_start_hits_ground_just_below_it() {
    let Some(maps) = load_all() else { return };
    let mut total = 0;
    for (name, map) in &maps {
        for (i, s) in map.player_starts.iter().enumerate() {
            let above = [s.position[0], s.position[1], s.position[2] + 1.0];
            let hit = map.collision.ray_down(above, 3.0).unwrap_or_else(|| {
                panic!("{name}: start {i} at {:?} finds no ground within 2 units below it", s.position)
            });
            // the ground is at most 2 units below the start and not above its eye line
            assert!(
                hit.z <= s.position[2] + 0.5 && hit.z >= s.position[2] - 2.0,
                "{name}: start {i}: ground at {} for a start at {}",
                hit.z,
                s.position[2]
            );

            // and the BSP agrees with a test that never looks at the BSP trees
            let reference = brute_force_ground(&map.collision, above, 3.0)
                .unwrap_or_else(|| panic!("{name}: start {i}: no polygon below it"));
            assert!((reference - hit.z).abs() < 1e-3, "{name}: start {i}: bsp {} vs polygons {reference}", hit.z);
            total += 1;
        }
    }
    assert_eq!(total, 829, "the 13 maps have 829 player starts in all");
}

#[test]
fn a_ray_from_below_the_ground_finds_nothing_facing_up() {
    let Some(maps) = load_all() else { return };
    for (name, map) in &maps {
        let s = map.player_starts[0];
        // 30 units below the start is inside the ground; a downward ray there must not
        // report the surface above it
        let below = [s.position[0], s.position[1], s.position[2] - 30.0];
        if let Some(hit) = map.collision.ray_down(below, 1.0) {
            assert!(hit.z < below[2], "{name}: a ray going down hit something above its start");
        }
    }
}

#[test]
fn placements_carry_sensible_fields() {
    let Some(maps) = load_all() else { return };
    for (name, map) in &maps {
        let b = map.world_bounds;
        let inside = |p: [f32; 3]| (0..3).all(|a| p[a] >= b[a * 2] - 1.0 && p[a] <= b[a * 2 + 1] + 1.0);
        let angle = |a: f32| a.is_finite() && a.abs() <= 2.0 * std::f32::consts::PI + 1e-3;

        for s in &map.player_starts {
            assert!(inside(s.position), "{name}: start outside the world: {:?}", s.position);
            assert!(angle(s.facing), "{name}: start facing {}", s.facing);
            // 0 and 1 are the two teams; Race starts number their lanes from 0 up
            assert!((0..16).contains(&s.team_index), "{name}: start team {}", s.team_index);
            assert!(s.game_types.iter().any(|&g| g != 0), "{name}: a start used by no game type");
        }
        for f in &map.netgame_flags {
            assert!(inside(f.position), "{name}: flag outside the world: {:?}", f.position);
            assert!(angle(f.facing), "{name}: flag facing {}", f.facing);
            assert!((0..=8).contains(&f.flag_type), "{name}: flag type {}", f.flag_type);
            // a team, hill or checkpoint number, by flag type
            assert!(f.team_index >= 0, "{name}: flag team {}", f.team_index);
        }
        assert!(!map.netgame_equipment.is_empty(), "{name}: no netgame equipment");
        for e in &map.netgame_equipment {
            assert!(inside(e.position), "{name}: equipment outside the world: {:?}", e.position);
            assert!(e.tag_name.ends_with(".itmc"), "{name}: equipment tag {:?}", e.tag_name);
        }
        for v in &map.vehicles {
            assert!(inside(v.position), "{name}: vehicle outside the world: {:?}", v.position);
            assert!(v.tag_name.ends_with(".vehi"), "{name}: vehicle tag {:?}", v.tag_name);
        }
    }
}

#[test]
fn blood_gulch_has_both_bases_and_its_warthogs() {
    let Some(dir) = map_dir() else { return };
    let map = HaloMap::from_path(map_path(&dir, "bloodgulch")).unwrap();

    let flags_of =
        |team| map.netgame_flags.iter().filter(|f| f.flag_type == flag_type::CTF_FLAG && f.team_index == team).count();
    assert_eq!((flags_of(0), flags_of(1)), (1, 1), "one CTF flag per team");

    let teams: Vec<i16> = map.player_starts.iter().map(|s| s.team_index).collect();
    assert!(teams.contains(&0) && teams.contains(&1));
    assert!(map.vehicles.iter().any(|v| v.tag_name.contains("warthog")), "{:?}", map.vehicles);
}

#[test]
fn every_map_has_the_pistol_and_the_players_body_as_the_tags_have_them() {
    let Some(maps) = load_all() else { return };
    for (name, map) in &maps {
        let c = &map.combat;
        let pistol = c.weapons.iter().find(|w| w.name == "weapons\\pistol\\pistol.weap").unwrap_or_else(|| {
            panic!("{name}: no pistol among {:?}", c.weapons.iter().map(|w| &w.name).collect::<Vec<_>>())
        });
        let t = &pistol.triggers[0];
        assert_eq!((t.initial_rate_of_fire, t.final_rate_of_fire), (3.5, 3.5), "{name}: the pistol's rate of fire");
        assert_eq!(pistol.magazines[t.magazine_index as usize].rounds_loaded_maximum, 12, "{name}");
        println!("{name}: pistol reload frames {} recoil frames {}", pistol.reload_frames, pistol.recoil_frames);
        assert!(pistol.reload_frames > 0, "{name}: the pistol's reload animation");
        let d =
            t.projectile.as_ref().and_then(|p| p.impact_damage).unwrap_or_else(|| panic!("{name}: no pistol damage"));
        assert_eq!((d.lower, d.upper), (25.0, 25.0), "{name}: what a pistol hit deals");
        assert!(d.flags & halo_map::combat::damage_flags::CAN_CAUSE_HEADSHOTS != 0, "{name}");

        let r = &c.resistance;
        assert_eq!(
            (r.maximum_body_vitality, r.maximum_shield_vitality),
            (75.0, 75.0),
            "{name}: the player's health and shield"
        );
        assert_eq!((r.shield_stun_time, r.shield_recharge_time), (6.0, 4.0), "{name}");
        assert!(r.shield_recharge_velocity > 0.0 && r.shield_recharge_velocity < 0.1, "{name}");
        assert!(r.materials.iter().any(|m| m.flags & halo_map::combat::MATERIAL_HEAD != 0), "{name}: a head");

        // what the byte form carries
        assert_eq!(&halo_map::combat::Combat::from_bytes(&c.to_bytes()).unwrap(), c, "{name}");
    }
}

#[test]
fn every_map_has_every_weapon_of_the_game_as_the_tags_have_it() {
    let Some(maps) = load_all() else { return };
    for (name, map) in &maps {
        let weapon = |weapon: &str| {
            map.combat
                .weapons
                .iter()
                .find(|w| w.name == format!("weapons\\{weapon}.weap"))
                .unwrap_or_else(|| panic!("{name}: no {weapon}"))
        };
        let trigger = |weapon: &halo_map::combat::Weapon, i: usize| weapon.triggers[i].clone();
        let projectile = |weapon: &halo_map::combat::Weapon, i: usize| trigger(weapon, i).projectile.unwrap();

        let rifle = weapon("assault rifle\\assault rifle");
        assert_eq!(trigger(rifle, 0).final_rate_of_fire, 15.0, "{name}");

        // a shotgun's shot is fifteen pellets, which slow down: the engine's timer starts that late
        let shotgun = weapon("shotgun\\shotgun");
        assert_eq!(trigger(shotgun, 0).projectiles_per_shot, 15, "{name}");
        let pellet = projectile(shotgun, 0);
        assert_eq!((pellet.air_damage_range_lower, pellet.air_damage_range_upper), (1.5, 3.0), "{name}");
        assert!(pellet.initial_velocity > pellet.final_velocity, "{name}");
        assert_eq!(shotgun.weapon_type, 1, "{name}");
        assert!(shotgun.shotgun_enter_frames > 0 && shotgun.reload_frames > 0, "{name}");
        let d = pellet.impact_damage.unwrap();
        assert_eq!((d.minimum, d.lower, d.upper), (8.0, 18.0, 25.0), "{name}");

        // a sniper rifle's trigger is latched, and its bullet does 101
        let sniper = weapon("sniper rifle\\sniper rifle");
        assert!(trigger(sniper, 0).flags & halo_map::combat::trigger_flags::LATCHED != 0, "{name}");
        assert_eq!(projectile(sniper, 0).impact_damage.unwrap().upper, 101.0, "{name}");

        // a plasma rifle heats, has a battery and misfires when it is old
        let plasma = weapon("plasma rifle\\plasma rifle");
        let t = trigger(plasma, 0);
        assert_eq!((t.heat_generated_per_round, t.age_generated_per_round), (0.08, 0.005), "{name}");
        assert!(plasma.age_misfire_start > 0.0 && plasma.age_heat_recovery_penalty > 0.0, "{name}");
        assert!(plasma.heat_overheated_threshold > plasma.heat_recovery_threshold, "{name}");

        // a plasma pistol charges its first trigger for 0.6 s, and the second is the overcharged bolt of 70
        let pistol = weapon("plasma pistol\\plasma pistol");
        assert_eq!(pistol.triggers.len(), 2, "{name}");
        assert_eq!(
            (trigger(pistol, 0).charging_time, trigger(pistol, 0).heat_generated_per_round),
            (0.6, 0.16),
            "{name}"
        );
        assert_eq!(trigger(pistol, 1).heat_generated_per_round, 1.0, "{name}");
        assert_eq!(projectile(pistol, 1).impact_damage.unwrap().upper, 70.0, "{name}");
        assert_eq!((pistol.weapon_type, pistol.secondary_trigger_mode), (3, 0), "{name}");

        // a rocket launcher's rocket makes an explosion (and no damage where it hits)
        let rocket = projectile(weapon("rocket launcher\\rocket launcher"), 0);
        assert!(rocket.impact_damage.is_none(), "{name}");
        let blast =
            rocket.detonation_damage.iter().find(|d| d.upper > 300.0).unwrap_or_else(|| panic!("{name}: a blast"));
        assert_eq!((blast.falloff_radius, blast.cutoff_radius, blast.core_radius), (0.5, 2.0, 0.6), "{name}");
        assert_eq!(rocket.maximum_range, 128.0, "{name}");

        // a needler's needle does nothing where it hits, hurts what it sticks to, and many make a blast together
        let needle = projectile(weapon("needler\\needler"), 0);
        assert_eq!(needle.impact_damage.unwrap().upper, 0.0, "{name}");
        assert_eq!(needle.attached_damage.unwrap().upper, 10.0, "{name}");
        assert!(needle.flags & halo_map::combat::projectile_flags::SUPER_COMBINING_EXPLOSION != 0, "{name}");
        assert!(needle.super_detonation_damage.iter().any(|d| d.upper == 60.0 && d.cutoff_radius == 1.0), "{name}");

        // every weapon has a melee blow, and the first-person animation it is timed by
        for w in [
            "assault rifle\\assault rifle",
            "pistol\\pistol",
            "shotgun\\shotgun",
            "sniper rifle\\sniper rifle",
            "plasma pistol\\plasma pistol",
            "plasma rifle\\plasma rifle",
        ] {
            let w = weapon(w);
            let blow = w.melee_damage.unwrap_or_else(|| panic!("{name}: {} has no melee damage", w.name));
            assert!(blow.lower > 0.0 && blow.cutoff_radius > 0.0, "{name}: {}", w.name);
            assert!(
                w.melee_frames > 0 && w.melee_key_frame > 0 && w.melee_key_frame < w.melee_frames,
                "{name}: {}",
                w.name
            );
        }
    }
}

#[test]
fn every_vehicle_placement_of_every_map_names_a_vehicle_that_is_read_whole() {
    use halo_map::vehicles::seat_flag;
    let Some(maps) = load_all() else { return };
    let mut placed = 0;
    for (name, map) in &maps {
        let v = &map.vehicle_tags;
        assert_eq!(v.placements.len(), map.vehicles.len(), "{name}: a vehicle tag for each placement");
        for (i, (placement, tag)) in map.vehicles.iter().zip(&v.placements).enumerate() {
            let tag = tag.unwrap_or_else(|| panic!("{name}: placement {i} ({}) names no vehicle", placement.tag_name));
            let def = v.def(tag).unwrap_or_else(|| panic!("{name}: placement {i}: no vehicle with tag {tag}"));
            assert_eq!(def.name, placement.tag_name, "{name}: placement {i}");

            let physics = def.physics.as_ref().unwrap_or_else(|| panic!("{name}: {} has no physics", def.name));
            assert!(physics.mass > 0.0 && !physics.mass_points.is_empty(), "{name}: {}", def.name);
            for m in &physics.mass_points {
                let p = m.powered_mass_point_index;
                assert!(p >= -1 && (p as i32) < physics.powered_mass_points.len() as i32, "{name}: {}", def.name);
            }
            assert!(!def.seats.is_empty(), "{name}: {} has no seats", def.name);
            assert!(
                def.seats.iter().any(|s| s.flags & (seat_flag::DRIVER | seat_flag::GUNNER) != 0),
                "{name}: {} has no driver's or gunner's seat",
                def.name
            );
            assert!(!def.weapons.is_empty(), "{name}: {} carries no weapon", def.name);
            for w in &def.weapons {
                assert!(map.combat.weapons.iter().any(|c| c.tag_index == *w), "{name}: {}: weapon {w}", def.name);
            }
            let body = def.resistance.as_ref().unwrap_or_else(|| panic!("{name}: {} has no collision", def.name));
            assert!(body.maximum_body_vitality > 0.0, "{name}: {}", def.name);
            placed += 1;
        }
        // what the byte form carries
        assert_eq!(&halo_map::vehicles::Vehicles::from_bytes(&v.to_bytes()).unwrap(), v, "{name}");
        v.check_against(&map.combat).unwrap_or_else(|e| panic!("{name}: {e}"));
    }
    assert!(placed > 0, "the 13 maps place some vehicles");
}

/// The Warthog, Ghost and Scorpion of Blood Gulch. What is known of them
/// without the numbers is asserted; the numbers themselves are printed (run
/// with `--nocapture`), for pinning with the developer's own game data.
#[test]
fn the_warthog_ghost_and_scorpion_of_blood_gulch_are_read_as_the_tags_have_them() {
    use halo_map::vehicles::{seat_flag, vehicle_type};
    let Some(dir) = map_dir() else { return };
    let map = HaloMap::from_path(map_path(&dir, "bloodgulch")).unwrap();
    let v = &map.vehicle_tags;
    let placed = |what: &str| {
        let tag = v.placements.iter().flatten().find(|t| v.def(**t).unwrap().name.contains(what));
        v.def(*tag.unwrap_or_else(|| panic!("no {what} placed on Blood Gulch"))).unwrap()
    };
    for (what, kind) in [
        ("warthog", vehicle_type::HUMAN_JEEP),
        ("ghost", vehicle_type::ALIEN_SCOUT),
        ("scorpion", vehicle_type::HUMAN_TANK),
    ] {
        let d = placed(what);
        println!("{what}: {:#?}", d);
        assert_eq!(d.handling.vehicle_type, kind, "{what}");
        assert!(d.handling.maximum_forward_speed > 0.0, "{what}");
        assert!(d.seats[0].flags & seat_flag::DRIVER != 0, "{what}: the first seat is the driver's");
        let p = d.physics.as_ref().unwrap();
        println!("{what}: {} powered and {} plain mass points", p.powered_mass_points.len(), p.mass_points.len());
    }
    let warthog = placed("warthog");
    assert!(warthog.seats.iter().any(|s| s.flags & seat_flag::GUNNER != 0), "a Warthog has a gunner's seat");
    println!("collision damage {:?}", v.collision_damage);
}
