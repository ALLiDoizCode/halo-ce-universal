//! The 500-player load for a running `halo-server`: simulated players that
//! take seats in the server's current match, speak UDP to its gateway the way
//! the game does, walk the map, shoot each other (so that kills, deaths, spawn
//! waves and the score limit all happen) and are measured as a client is.
//!
//!   halo-slayer-load --spacetimedb http://127.0.0.1:3000 --maps <folder with the .map files> \
//!       --players 500 --out target/slayer-check
//!
//! (See `rust/halo-server/check/run.sh` for the whole run: SpacetimeDB, the
//! server, this program and the game, each pinned to its own cores.)
//!
//! Options, each `--name value`:
//!
//! - `--spacetimedb http://127.0.0.1:3000`, `--root halo-root`, `--server lounge`:
//!   where the server list is; the load joins the first match that server lists
//!   after it starts (so start it before the match you want, or between matches).
//! - `--maps <dir>`: the `.map` files; the walkers walk the match's map on the
//!   same collision data the server judges their moves with.
//! - `--players 500`: seats taken (ids `0..players`, so the match must be empty:
//!   a real client that joins afterwards is player `players`).
//! - `--shots 0.5`: shots a second each player with a target in range fires, `--range 25`
//!   world units; `--shots 0` for a match nobody fights.
//! - `--hunt 150`: a player with nobody in range walks towards the nearest enemy within this
//!   many world units (0: they wander), so that a crowd keeps meeting.
//! - `--guest 500`: the id of a player (the real game, which joins after the simulated
//!   players and is player `--players`) whom everyone else walks to and nobody shoots, so
//!   that the crowd is around the game and in its view.
//! - `--budget 90000`: the budget the server was given, for the report to compare with.
//! - `--loss 0`, `--delay-ms 0`: the simulated players' links (each way).
//! - `--window 30`: seconds per window of the report.
//! - `--hold 45`: seconds the simulated players stay seated after the game
//!   ends (the final scoreboard is up; a real client takes its screenshot).
//! - `--owner-token-file <file>`: the server's `owner.token` (beside its configuration), with
//!   which the report reads the private `shooter` table: why the server refused hit reports.
//! - `--out <dir>`: the report is also written there as `load-report.txt`.
//!
//! The report: the seats and the join; ticks the module ran per wall-clock
//! second (the worst seconds); for each window and the whole match, the
//! download per player against the budget, ticks missed, how old a tick was on
//! arrival, update rates and longest gaps by distance, and how stale any
//! player's state got; the fight (hits sent and refused, kills, deaths); and
//! the final scoreboard; the rules in force; and the time out of the world, with a respawn after a
//! death split into its timer and the wait after it, how each was placed (a free start, beside
//! one, a wave) and how many starting locations were free for each team.

use std::collections::{BTreeMap, HashMap};
use std::net::ToSocketAddrs;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use halo_gateway::harness::{analyze, sim_public_key, Crowd, Impairment, Truth, DEFAULT_BANDS};
use halo_match_driver::module_bindings::report_hits as _;
use halo_match_driver::walkers::Walkers;
use halo_match_driver::{FighterRow, MatchClient, PlayerClient, SeenTick, StandingRow};
use halo_server::admin::Admin;
use halo_server::loadgen::{free_starts, spread, Contact, Gunner, Path, Respawns, Seen};
use halo_server::root::Root;
use halo_sim::combat::HitReport;
use halo_sim::spawn::Occupant;
use halo_sim::wire::encode_hits;
use halo_sim::{MapData, TICKS_PER_SECOND};
use spacetimedb_sdk::Identity;

fn main() {
    let args: HashMap<String, String> = std::env::args()
        .skip(1)
        .collect::<Vec<_>>()
        .chunks(2)
        .filter_map(|c| Some((c.first()?.trim_start_matches("--").to_string(), c.get(1)?.clone())))
        .collect();
    let get = |k: &str, d: &str| args.get(k).cloned().unwrap_or_else(|| d.to_string());
    let url = get("spacetimedb", "http://127.0.0.1:3000");
    let root_database = get("root", "halo-root");
    let server_id = get("server", "lounge");
    let maps = PathBuf::from(args.get("maps").unwrap_or_else(|| fail("--maps <the folder with the .map files>")));
    let players: u16 = get("players", "500").parse().unwrap_or_else(|_| fail("--players is a number"));
    let shots: f32 = get("shots", "0.5").parse().unwrap_or_else(|_| fail("--shots is a number"));
    let range: f32 = get("range", "25").parse().unwrap_or_else(|_| fail("--range is a number"));
    let guest: Option<u16> = args.get("guest").map(|g| g.parse().unwrap_or_else(|_| fail("--guest is a player id")));
    let hunt: f32 = get("hunt", "150").parse().unwrap_or_else(|_| fail("--hunt is a number"));
    let loss: f32 = get("loss", "0").parse().unwrap_or_else(|_| fail("--loss is a number"));
    let delay_ms: u64 = get("delay-ms", "0").parse().unwrap_or_else(|_| fail("--delay-ms is a number"));
    let window_secs: u32 = get("window", "30").parse().unwrap_or_else(|_| fail("--window is a number"));
    let hold: u64 = get("hold", "45").parse().unwrap_or_else(|_| fail("--hold is a number"));
    let budget: f64 = get("budget", "90000").parse().unwrap_or_else(|_| fail("--budget is a number"));
    let out = args.get("out").map(PathBuf::from);
    let owner_token_file = args.get("owner-token-file").cloned();

    let admin = Admin::new(&url).unwrap_or_else(|e| fail(&e));
    let me = admin.new_identity().unwrap_or_else(|e| fail(&e));
    // (the server publishes the root database when it starts: wait for it)
    let root = wait_for(Duration::from_secs(120), || Root::connect(&url, &root_database, &me.token).ok())
        .unwrap_or_else(|| fail("the root database never came up"));

    say("waiting for the server to list a match");
    let row = wait_for(Duration::from_secs(600), || {
        root.servers().into_iter().find(|s| s.id == server_id && !s.database.is_empty())
    })
    .unwrap_or_else(|| fail("no match was listed"));
    say(&format!(
        "joining match {} ({} on {}): database {}, gateway {}, up to {} players",
        row.match_number, row.game_type, row.map, row.database, row.gateway, row.capacity
    ));
    let teams = row.game_type.contains("team");
    let gateway = row
        .gateway
        .to_socket_addrs()
        .ok()
        .and_then(|mut a| a.next())
        .unwrap_or_else(|| fail(&format!("the gateway address {:?} does not resolve", row.gateway)));

    let halo_map =
        halo_map::HaloMap::from_path(maps.join(format!("{}.map", row.map))).unwrap_or_else(|e| fail(&e.to_string()));
    let anchors: Vec<[f32; 3]> = halo_map.player_starts.iter().map(|s| s.position).collect();
    let map = MapData::from(halo_map);
    let (mut walkers, _spawn) = Walkers::new(map, &anchors, players, 7);

    let watcher = MatchClient::try_connect_as(&url, &row.database, None).unwrap_or_else(|e| fail(&e));
    let capacity = players as usize + 12;

    // the seats, one after another: player n takes seat n
    let joining = Instant::now();
    let mut seats: Vec<PlayerClient> = Vec::new();
    let mut identities: Vec<Identity> = Vec::new();
    for id in 0..players {
        let account = admin.new_identity().unwrap_or_else(|e| fail(&e));
        let client = PlayerClient::connect_unsubscribed(&url, &row.database, &account.token);
        client.join(sim_public_key(id)).unwrap_or_else(|e| fail(&format!("player {id} joining: {e}")));
        identities.push(Identity::from_hex(&account.identity).unwrap());
        seats.push(client);
    }
    for (id, identity) in identities.iter().enumerate() {
        let held = wait_for(Duration::from_secs(60), || {
            (watcher.seats().get(&(id as u16)).map(|s| s.owner) == Some(*identity)).then_some(())
        });
        if held.is_none() {
            fail(&format!("seat {id} did not appear as the one for its identity (was the match empty?)"));
        }
    }
    say(&format!("{players} seats taken in {:.1} s", joining.elapsed().as_secs_f64()));

    let link = Impairment::loss(loss).delayed(Duration::from_millis(delay_ms));
    let crowd = Crowd::connect(gateway, 0..players, link, capacity, false);
    let udp = Instant::now();
    crowd.join_all(Duration::from_secs(120)).unwrap_or_else(|e| fail(&e));
    say(&format!("{players} players welcomed over UDP in {:.1} s", udp.elapsed().as_secs_f64()));

    let mut truth = Truth::new(capacity);
    let mut gunner = Gunner::new(capacity, range, shots, teams, 11).with_hunt_range(hunt);
    if let Some(guest) = guest {
        gunner = gunner.with_guest(guest);
    }
    let mut seen_ticks: Vec<(u64, i64)> = Vec::new();
    // refused moves, by how long after the player spawned they were refused (ticks): 0-3, 4-30, later
    let mut refused_after_spawn = [0u64; 3];
    let mut refusals_seen: HashMap<u16, u64> = HashMap::new();
    // how long players wait to be in the world: for a joining player's first spawn, and for the
    // respawns after a death, split into the respawn timer and the wait after it
    let mut respawns = Respawns::default();
    let starts: Vec<[f32; 3]> = walkers.map.starts.iter().map(|s| s.position).collect();
    // starting locations free for each team, sampled once a second: (red, blue) of each sample
    let mut free_samples: Vec<[usize; 2]> = Vec::new();
    let mut next_sample = 0u64;
    let mut rules_in_force = None;
    let mut fired = 0u64;
    let mut ended_at: Option<Instant> = None;
    let started = Instant::now();
    let mut last_progress = Instant::now();
    watcher.discard_ticks();
    loop {
        let Some(mut seen) = watcher.next_tick(Duration::from_secs(10)) else {
            say("no tick for 10 s: the match is gone");
            break;
        };
        truth.record(&seen);
        seen_ticks.push((seen.marker.tick, seen.marker.stamped_us));
        while let Some(newer) = watcher.next_tick(Duration::ZERO) {
            truth.record(&newer);
            seen_ticks.push((newer.marker.tick, newer.marker.stamped_us));
            seen = newer;
        }
        let game = watcher.game();
        if rules_in_force.is_none() {
            rules_in_force = game.clone();
        }
        if game.as_ref().is_some_and(|g| g.ending != 0) && ended_at.is_none() {
            ended_at = Some(Instant::now());
            say(&format!("the game has ended at tick {}; the final scoreboard is up", seen.marker.tick));
        }
        if ended_at.is_some_and(|at| at.elapsed() >= Duration::from_secs(hold)) {
            break;
        }
        let standings = watcher.standings();
        let fighters = watcher.fighters();
        for standing in standings.values() {
            let spawn = [standing.x, standing.y, standing.z];
            let (player, state, due_tick, spawns, spawned_tick) =
                (standing.player, standing.state, standing.due_tick, standing.spawns, standing.spawned_tick);
            respawns.observe(seen.marker.tick, Seen { player, state, due_tick, spawns, spawned_tick, spawn }, &starts);
        }
        if seen.marker.tick >= next_sample && ended_at.is_none() {
            next_sample = seen.marker.tick + TICKS_PER_SECOND as u64;
            let others: Vec<Occupant> = standings
                .values()
                .filter(|s| s.state == 0)
                .filter_map(|s| {
                    seen.players.get(&s.player).map(|p| Occupant { position: [p.x, p.y, p.z], team: s.team })
                })
                .collect();
            free_samples.push(free_starts(&walkers.map, teams, &others));
        }
        for row in seen.players.values() {
            let before = refusals_seen.insert(row.id, row.rejected_moves).unwrap_or(0);
            if row.rejected_moves > before {
                let since =
                    standings.get(&row.id).map_or(u64::MAX, |s| seen.marker.tick.saturating_sub(s.spawned_tick));
                refused_after_spawn[if since <= 3 {
                    0
                } else if since <= 30 {
                    1
                } else {
                    2
                }] += row.rejected_moves - before;
            }
        }
        walkers.sync_with_server(seen.players.values());
        // (everyone sends an input every tick, the dead too: the gateway lets go of a player
        // who is silent for 10 s, and the game keeps its player's session alive the same way;
        // the rules ignore the moves of those who are not alive)
        let mut inputs = walkers.next_inputs();
        for input in &mut inputs {
            // (a dead player stays where they fell: the game's keepalive repeats its unit's place)
            if standings.get(&input.player).is_some_and(|s| s.state != 0) {
                if let Some(row) = seen.players.get(&input.player) {
                    (input.position, input.yaw, input.pitch) = ([row.x, row.y, row.z], row.yaw, row.pitch);
                }
            }
        }
        crowd.send_inputs(&inputs);
        if shots > 0.0 && ended_at.is_none() {
            let contacts = contacts_of(&seen, &standings, &fighters, capacity);
            fired += fire(&mut gunner, &contacts, &seen, &fighters, &walkers.map, &seats);
            if hunt > 0.0 {
                for (player, heading) in gunner.hunt(seen.marker.tick, &contacts) {
                    walkers.set_course(player, 1.0, heading);
                }
            }
        }
        if last_progress.elapsed() >= Duration::from_secs(10) {
            last_progress = Instant::now();
            progress(&watcher, started.elapsed(), fired, gunner.engaged());
        }
    }
    let finished = Instant::now();
    // (read now: the server writes the file when it starts, which is after this program does)
    let owner_token = owner_token_file.and_then(|f| std::fs::read_to_string(f).ok()).map(|t| t.trim().to_string());
    let refusals = owner_token.as_ref().map(|token| {
        admin.sql(&row.database, "SELECT last_reject, rejected, accepted FROM shooter", token).unwrap_or_else(|e| e)
    });
    say("analysing");

    let mut report = String::new();
    let mut line = |text: String| {
        println!("{text}");
        report.push_str(&text);
        report.push('\n');
    };
    line(format!(
        "== {} players on {} ({}), budget {budget} B/s, loss {loss} delay {delay_ms} ms, shots {shots}/s range {range} wu ==",
        players, row.map, row.game_type
    ));
    line(format!(
        "match ran {:.1} s of load; {fired} hit reports sent",
        finished.duration_since(started).as_secs_f64()
    ));
    ticks_per_second(&seen_ticks, &mut line);

    let window_ticks = (window_secs * TICKS_PER_SECOND) as usize;
    let all: Vec<u32> = truth.ticks.keys().copied().collect();
    let mut worst = (0.0f64, 0u32);
    for (n, chunk) in all.chunks(window_ticks.max(1)).enumerate() {
        if chunk.len() < window_ticks / 2 {
            continue;
        }
        let range = chunk[0] + 3..chunk[chunk.len() - 1].saturating_sub(2);
        let r = analyze(&crowd, &truth, range, &DEFAULT_BANDS);
        line(format!("-- window {n}: ticks {}..{} --", chunk[0], chunk[chunk.len() - 1]));
        line(r.to_string().trim_end().to_string());
        if r.max_download > worst.0 {
            worst = (r.max_download, n as u32);
        }
    }
    line(format!(
        "highest download of any player in any window: {:.1} KB/s (window {}), the budget is {:.1} KB/s: {}",
        worst.0 / 1e3,
        worst.1,
        budget / 1e3,
        if worst.0 <= budget * 1.001 { "within it" } else { "OVER it" }
    ));
    if all.len() > 2 * window_ticks.max(1) {
        let r = analyze(&crowd, &truth, all[3]..all[all.len() - 3], &DEFAULT_BANDS);
        line("-- the whole of it --".to_string());
        line(r.to_string().trim_end().to_string());
    }

    scoreboard(&watcher, &mut line);
    if let Some(g) = &rules_in_force {
        let secs = |t: u32| t as f64 / TICKS_PER_SECOND as f64;
        line(format!(
            "rules in force: respawn time {:.1} s (growth {:.1} s), suicide penalty {:.1} s, wave interval {:.1} s, \
             score limit {}, time limit {:.0} s, {}",
            secs(g.respawn_ticks),
            secs(g.respawn_growth_ticks),
            secs(g.suicide_penalty_ticks),
            secs(g.wave_ticks),
            g.score_limit,
            secs(g.time_limit_ticks),
            if g.teams { "teams" } else { "no teams" }
        ));
    }
    if let Some([median, p90, p99, longest]) = spread(&mut respawns.first) {
        line(format!(
            "time out of the world, a joining player's first spawn: {} waits, median {median:.1} s  p90 {p90:.1}  p99 {p99:.1}  longest {longest:.1} s",
            respawns.first.len()
        ));
    }
    let mut totals: Vec<f64> = respawns.respawns.iter().map(|r| r.timer + r.after).collect();
    let mut timers: Vec<f64> = respawns.respawns.iter().map(|r| r.timer).collect();
    let mut afters: Vec<f64> = respawns.respawns.iter().map(|r| r.after).collect();
    for (what, values) in [
        ("whole, from death to spawn", &mut totals),
        ("the respawn timer, with any penalty", &mut timers),
        ("the wait after the timer ran out", &mut afters),
    ] {
        if let Some([median, p90, p99, longest]) = spread(values) {
            line(format!(
                "respawn after a death, {what}: {} respawns, median {median:.2} s  p90 {p90:.2}  p99 {p99:.2}  longest {longest:.2} s",
                values.len()
            ));
        }
    }
    if !respawns.respawns.is_empty() {
        line(format!(
            "respawns by how they were placed: at a free start {}, beside a start at once {}, in a wave {}",
            respawns.by_path(Path::FreeStart),
            respawns.by_path(Path::BesideStart),
            respawns.by_path(Path::Wave)
        ));
        let mut longest: Vec<_> = respawns.respawns.iter().filter(|r| r.after > 1.0).collect();
        longest.sort_by(|a, b| b.after.partial_cmp(&a.after).unwrap());
        let unaccounted = longest.iter().filter(|r| r.path != Path::Wave).count();
        line(format!(
            "respawns that waited over 1 s after the timer: {}, of which not in a wave {unaccounted}",
            longest.len()
        ));
    }
    if !free_samples.is_empty() {
        let n = free_samples.len() as f64;
        let of = |team: usize| {
            let counts: Vec<usize> = free_samples.iter().map(|s| s[team]).collect();
            (
                counts.iter().sum::<usize>() as f64 / n,
                counts.iter().min().copied().unwrap(),
                counts.iter().max().copied().unwrap(),
            )
        };
        let total = walkers.map.starts.iter().filter(|s| s.is_for_slayer()).count();
        for (team, name) in ["red", "blue"].into_iter().enumerate() {
            let (mean, low, high) = of(team);
            line(format!(
                "starting locations free for {name}, of {total}, sampled each second ({} samples): mean {mean:.1}  least {low}  most {high}",
                free_samples.len()
            ));
        }
    }
    line(format!(
        "refused moves by the ticks since the player spawned: 0 to 3 {}, 4 to 30 {}, later {}",
        refused_after_spawn[0], refused_after_spawn[1], refused_after_spawn[2]
    ));
    if let Some(answer) = refusals {
        line(format!("hit reports by shooter: {}", summarise_shooters(&answer)));
    }
    if let Some(dir) = out {
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(dir.join("load-report.txt"), report);
    }
    drop(crowd);
    for seat in &seats {
        seat.disconnect();
    }
}

/// Who is where, on which team, and alive.
fn contacts_of(
    seen: &SeenTick,
    standings: &BTreeMap<u16, StandingRow>,
    fighters: &BTreeMap<u16, FighterRow>,
    capacity: usize,
) -> Vec<Option<Contact>> {
    let mut contacts: Vec<Option<Contact>> = vec![None; capacity];
    for (id, row) in &seen.players {
        let Some(slot) = contacts.get_mut(*id as usize) else { continue };
        let standing = standings.get(id);
        *slot = Some(Contact {
            position: [row.x, row.y, row.z],
            team: standing.map_or(0, |s| s.team),
            alive: standing.is_some_and(|s| s.state == 0) && fighters.contains_key(id),
        });
    }
    contacts
}

/// Fire this tick's shots from the simulated players' own connections.
fn fire(
    gunner: &mut Gunner,
    contacts: &[Option<Contact>],
    seen: &SeenTick,
    fighters: &BTreeMap<u16, FighterRow>,
    map: &MapData,
    seats: &[PlayerClient],
) -> u64 {
    let mut batches: HashMap<u16, Vec<HitReport>> = HashMap::new();
    for (shooter, target) in gunner.shots(seen.marker.tick, contacts) {
        let Some(damage) = fighters.get(&shooter).and_then(|f| impact_damage(map, f.weapon_0)) else { continue };
        let Some(at) = seen.players.get(&target) else { continue };
        batches.entry(shooter).or_default().push(HitReport {
            target,
            damage,
            material: 1,
            scale: 1.0,
            host_tick: seen.marker.tick as u32,
            origin: [at.x, at.y, at.z + 0.3],
            target_position: [at.x, at.y, at.z],
        });
    }
    let mut count = 0;
    for (shooter, hits) in batches {
        // (only simulated players have a connection here; the game's own shoots for itself)
        if let Some(seat) = seats.get(shooter as usize) {
            count += hits.len() as u64;
            let _ = seat.conn.reducers.report_hits(encode_hits(&hits));
        }
    }
    count
}

/// The damage tag of the weapon's first trigger's bullet: what a hit with it names.
fn impact_damage(map: &MapData, weapon: u16) -> Option<u16> {
    map.combat.weapon(weapon)?.triggers.iter().find_map(|t| t.projectile.as_ref()?.impact_damage.map(|d| d.tag_index))
}

fn progress(watcher: &MatchClient, elapsed: Duration, fired: u64, engaged: usize) {
    let (Some(marker), Some(game)) = (watcher.marker(), watcher.game()) else { return };
    let alive = watcher.standings().values().filter(|s| s.state == 0).count();
    let armed = watcher.fighters().values().filter(|f| f.weapon_0 != u16::MAX).count();
    // where the living stand: the busiest 40-unit squares, as (x, y): red + blue
    let teams: BTreeMap<u16, u8> = watcher.standings().values().map(|s| (s.player, s.team)).collect();
    let mut squares: BTreeMap<(i32, i32), (u32, u32)> = BTreeMap::new();
    for p in watcher.players().values() {
        let entry = squares.entry(((p.x / 40.0).floor() as i32 * 40, (p.y / 40.0).floor() as i32 * 40)).or_default();
        if teams.get(&p.id) == Some(&0) {
            entry.0 += 1;
        } else {
            entry.1 += 1;
        }
    }
    let mut busiest: Vec<_> = squares.into_iter().collect();
    busiest.sort_by_key(|(_, (r, b))| std::cmp::Reverse(r + b));
    say(&format!("      busiest squares {:?}", &busiest[..busiest.len().min(5)]));
    say(&format!(
        "{:>4.0} s  tick {}  players {}  alive {alive}  armed {armed}  shooting at someone {engaged}  red {} blue {}  hits sent {fired}  refused {}  moves refused {}",
        elapsed.as_secs_f64(),
        marker.tick,
        marker.players,
        game.red_score,
        game.blue_score,
        marker.rejected_hits_total,
        marker.rejected_total
    ));
}

/// How many ticks the module ran in each wall-clock second (by its own stamps),
/// and the longest time between two.
fn ticks_per_second(ticks: &[(u64, i64)], line: &mut impl FnMut(String)) {
    let mut by_second: BTreeMap<i64, u32> = BTreeMap::new();
    let mut seen = std::collections::BTreeSet::new();
    let mut gaps = Vec::new();
    let mut previous: Option<i64> = None;
    for (tick, at) in ticks {
        if !seen.insert(*tick) {
            continue;
        }
        *by_second.entry(at / 1_000_000).or_default() += 1;
        if let Some(p) = previous {
            gaps.push((at - p) as f64 / 1e3);
        }
        previous = Some(*at);
    }
    // (the first and last seconds are partial)
    let whole: Vec<(i64, u32)> =
        by_second.iter().skip(1).take(by_second.len().saturating_sub(2)).map(|(s, n)| (*s, *n)).collect();
    if whole.is_empty() {
        return;
    }
    let mut counts: Vec<u32> = whole.iter().map(|(_, n)| *n).collect();
    counts.sort_unstable();
    gaps.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let pick = |q: f64| gaps[((gaps.len() as f64 - 1.0) * q) as usize];
    // (a second's worth of ticks by wall-clock second jitters between 29 and 31 with the
    // stamps of a steady 33.3 ms cadence, so the worst seconds are judged with the gaps)
    let first = ticks.iter().min_by_key(|(t, _)| *t).copied().unwrap_or((0, 0));
    let last = ticks.iter().max_by_key(|(t, _)| *t).copied().unwrap_or((0, 0));
    let overall = (last.0 - first.0) as f64 / ((last.1 - first.1) as f64 / 1e6).max(1e-9);
    line(format!(
        "ticks per second      {} seconds measured; fewest in one {}, most {}; {:.3} ticks a second over the whole of it ({} ticks in {:.1} s, {} tick numbers never seen)",
        counts.len(),
        counts[0],
        counts[counts.len() - 1],
        overall,
        last.0 - first.0,
        (last.1 - first.1) as f64 / 1e6,
        (last.0 - first.0 + 1).saturating_sub(seen.len() as u64)
    ));
    line(format!(
        "time between ticks    p50 {:.2}  p99 {:.2}  p99.9 {:.2}  max {:.2} ms (the module's own stamps); of {} gaps: over 40 ms {}, over 50 ms {}, over 100 ms {}",
        pick(0.5),
        pick(0.99),
        pick(0.999),
        gaps[gaps.len() - 1],
        gaps.len(),
        gaps.iter().filter(|g| **g > 40.0).count(),
        gaps.iter().filter(|g| **g > 50.0).count(),
        gaps.iter().filter(|g| **g > 100.0).count()
    ));
}

fn scoreboard(watcher: &MatchClient, line: &mut impl FnMut(String)) {
    let (Some(game), Some(marker)) = (watcher.game(), watcher.marker()) else { return };
    let standings = watcher.standings();
    let kills: i64 = standings.values().map(|s| s.score as i64).sum();
    let deaths: u64 = standings.values().map(|s| s.deaths as u64).sum();
    line(format!(
        "final: ending {} (0 not ended, 1 score limit, 2 time), winner kind {} id {}; red {} blue {}; score limit {}; {} players, {} deaths in all, sum of scores {kills}; hits refused {} of the tick markers, moves refused {}",
        game.ending, game.winner_kind, game.winner, game.red_score, game.blue_score, game.score_limit, standings.len(), deaths,
        marker.rejected_hits_total, marker.rejected_total
    ));
    let mut moves: BTreeMap<&str, (u32, u64)> = BTreeMap::new();
    for p in watcher.players().values().filter(|p| p.rejected_moves > 0) {
        let why = match p.last_reject {
            1 => "unknown player",
            2 => "not finite",
            3 => "too fast",
            4 => "through a surface",
            5 => "off the ground",
            6 => "duplicate input",
            _ => "other",
        };
        let entry = moves.entry(why).or_default();
        entry.0 += 1;
        entry.1 += p.rejected_moves;
    }
    line(format!("refused moves by the reason of each player's last refusal (players, moves): {moves:?}"));
    let roster = watcher.roster();
    let mut top: Vec<_> = standings.values().collect();
    top.sort_by_key(|s| std::cmp::Reverse(s.score));
    for s in top.iter().take(5) {
        let name = roster.get(&s.player).map_or("?", |r| r.name.as_str());
        line(format!("  player {:>3} ({name}) team {} score {} deaths {}", s.player, s.team, s.score, s.deaths));
    }
}

/// The `shooter` table's answer, as totals: hits accepted and refused, and how many
/// shooters last had each reason (codes of `halo_sim::combat::Reject::code`).
fn summarise_shooters(answer: &str) -> String {
    let Some(rows) = answer.split("\"rows\":").nth(1) else { return answer.chars().take(200).collect() };
    let rows = rows.split("]]").next().unwrap_or("").trim_start_matches('[');
    let (mut accepted, mut rejected) = (0u64, 0u64);
    let mut reasons: BTreeMap<u64, u64> = BTreeMap::new();
    for row in rows.split("],[") {
        let cells: Vec<u64> =
            row.trim_matches(|c| c == '[' || c == ']').split(',').filter_map(|c| c.trim().parse().ok()).collect();
        if let [reason, r, a] = cells[..] {
            rejected += r;
            accepted += a;
            if r > 0 {
                *reasons.entry(reason).or_default() += 1;
            }
        }
    }
    format!("{accepted} accepted, {rejected} refused; shooters by the reason of their last refusal {reasons:?}")
}

fn wait_for<T>(timeout: Duration, mut f: impl FnMut() -> Option<T>) -> Option<T> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(v) = f() {
            return Some(v);
        }
        if Instant::now() > deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn say(message: &str) {
    eprintln!("[load] {message}");
}

fn fail(message: &str) -> ! {
    eprintln!("halo-slayer-load: {message}");
    std::process::exit(2);
}
