//! Who shoots whom in a simulated match: the part of the 500-player load
//! generator (`halo-slayer-load`) that decides, each tick, which simulated
//! players fire at which others, so that kills happen and the match can end
//! on its score limit.
//!
//! A [`Gunner`] gives each player a target: the nearest living enemy within
//! its range, kept while it lives and stays in range, then looked for again. A
//! player with a target fires a shot every `1 / shots_per_second` seconds on
//! average (a little different each time). It plays like a fight: players near
//! each other and not on one team shoot at each other until one falls.

use std::collections::HashMap;
use std::sync::Arc;

use halo_sim::gametype::GameType as _;
use halo_sim::Rng;
use halo_sim::TICKS_PER_SECOND;

use crate::nav::{Nav, Scratch};

/// A player as the gunners see them.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Contact {
    pub position: [f32; 3],
    pub team: u8,
    pub alive: bool,
}

pub struct Gunner {
    rng: Rng,
    range: f32,
    teams: bool,
    mean_gap_ticks: f32,
    /// Per player id: who they are shooting at.
    targets: Vec<Option<u16>>,
    /// Per player id: the tick they fire next at the earliest.
    next_shot: Vec<u64>,
    /// How far a player looks for an enemy to walk towards, when none is in range.
    hunt_range: f32,
    /// Per player id: where they were when last looked at, and when.
    anchors: Vec<Option<([f32; 3], u64)>>,
    /// Per player id: until when they walk the way they were turned (a wall was in the way).
    detour_until: Vec<u64>,
    /// A player everyone else walks to and nobody shoots (the real game, in the load's
    /// measurements of it: the crowd is around it, and in its view).
    guest: Option<u16>,
    /// The map's walkable ground, to steer hunters over ([`Gunner::with_nav`]).
    nav: Option<Arc<Nav>>,
    scratch: Scratch,
    /// How many courses were set along a path, and how many straight at the target, as a grid
    /// was asked (see [`Gunner::course_counts`]).
    along_path: u64,
    straight: u64,
}

/// How often a hunter is looked at to see whether they are getting anywhere.
const PROGRESS_TICKS: u64 = 2 * TICKS_PER_SECOND as u64;
/// The least a hunter must have moved in that time, in world units, not to be taken to be stuck.
const PROGRESS_UNITS: f32 = 2.0;
/// How long a stuck hunter walks the way they are turned, before hunting again.
const DETOUR_TICKS: u64 = 3 * TICKS_PER_SECOND as u64;

impl Gunner {
    /// `players`: one more than the largest player id; `range` in world
    /// units; `teams`: whether players of one team spare each other.
    pub fn new(players: usize, range: f32, shots_per_second: f32, teams: bool, seed: u64) -> Gunner {
        Gunner {
            rng: Rng::seeded(seed),
            range,
            teams,
            mean_gap_ticks: TICKS_PER_SECOND as f32 / shots_per_second.max(1e-3),
            targets: vec![None; players],
            next_shot: vec![0; players],
            hunt_range: 150.0,
            anchors: vec![None; players],
            detour_until: vec![0; players],
            guest: None,
            nav: None,
            scratch: Scratch::default(),
            along_path: 0,
            straight: 0,
        }
    }

    /// Steer hunters along paths over the map's walkable ground, round walls and cliffs, and
    /// no longer straight at their target. Where there is no path (or a hunter or their target is
    /// off the grid) they walk straight at it, as they do without a grid.
    pub fn with_nav(mut self, nav: Arc<Nav>) -> Gunner {
        self.nav = Some(nav);
        self
    }

    /// The direction a hunter at `from` walks in to get to `to`: along the grid's path if
    /// there is one, else straight (radians in the map's plane, from the x axis towards y).
    fn course(&mut self, from: [f32; 3], to: [f32; 3]) -> f32 {
        let along = self.nav.as_ref().and_then(|nav| nav.heading(&mut self.scratch, from, to));
        if self.nav.is_some() {
            *if along.is_some() { &mut self.along_path } else { &mut self.straight } += 1;
        }
        along.unwrap_or_else(|| (to[1] - from[1]).atan2(to[0] - from[0]))
    }

    /// How many courses were set so far along a path of the grid, and how many straight at the
    /// target because the grid had no path (both 0 without a grid).
    pub fn course_counts(&self) -> (u64, u64) {
        (self.along_path, self.straight)
    }

    /// How many players have someone to shoot at, as of the last tick.
    pub fn engaged(&self) -> usize {
        self.targets.iter().filter(|t| t.is_some()).count()
    }

    /// Let everyone with nobody to shoot walk to this player, within the hunt range,
    /// to stand around them, and let nobody shoot them.
    pub fn with_guest(mut self, guest: u16) -> Gunner {
        self.guest = Some(guest);
        self
    }

    /// The contacts as the targeting sees them: without the guest, who is nobody's enemy.
    fn without_guest<'a>(&self, contacts: &'a [Option<Contact>]) -> std::borrow::Cow<'a, [Option<Contact>]> {
        match self.guest {
            Some(g) if contacts.get(g as usize).is_some_and(|c| c.is_some()) => {
                let mut copy = contacts.to_vec();
                copy[g as usize] = None;
                std::borrow::Cow::Owned(copy)
            }
            _ => std::borrow::Cow::Borrowed(contacts),
        }
    }

    /// How far (world units) a player with nobody in range looks for an enemy to walk towards.
    pub fn with_hunt_range(mut self, hunt_range: f32) -> Gunner {
        self.hunt_range = hunt_range;
        self
    }

    /// The directions to walk in, for the players who have nobody to shoot at: towards
    /// the nearest enemy within the hunt range (along a path, with a grid: see
    /// [`Gunner::with_nav`]), decided once a second for each player
    /// (on the tick of their id). The angle is of the direction in the map's plane, from the
    /// x axis towards the y axis, which is how the walkers' headings are measured.
    pub fn hunt(&mut self, tick: u64, contacts: &[Option<Contact>]) -> Vec<(u16, f32)> {
        let guest = self.guest.and_then(|g| contacts.get(g as usize).copied().flatten()).filter(|g| g.alive);
        let contacts = &*self.without_guest(contacts);
        let grid = Grid::of(contacts, self.hunt_range);
        let mut headings = Vec::new();
        for id in 0..self.targets.len().min(contacts.len()) {
            let Some(me) = contacts[id].filter(|c| c.alive) else {
                self.anchors[id] = None;
                continue;
            };
            if self.targets[id].is_some() {
                self.anchors[id] = None;
                continue;
            }
            // (one who is walking the way they were turned is not looked at: they are looked
            // at again from where they are when the detour is over. Looked at during it, a hunter
            // who cannot move is found stuck every two seconds, and never gets a course again)
            if tick < self.detour_until[id] {
                self.anchors[id] = None;
                continue;
            }
            // (walkers turn away from an edge and a drop, but not from a wall they are held
            // off by: one who has got nowhere in a couple of seconds turns another way)
            match self.anchors[id] {
                Some((from, at)) if tick >= at + PROGRESS_TICKS => {
                    self.anchors[id] = Some((me.position, tick));
                    if distance_squared(from, me.position) < PROGRESS_UNITS * PROGRESS_UNITS {
                        self.detour_until[id] = tick + DETOUR_TICKS;
                        headings.push((id as u16, self.rng.next_f32() * core::f32::consts::TAU));
                        continue;
                    }
                }
                None => self.anchors[id] = Some((me.position, tick)),
                _ => {}
            }
            if tick < self.detour_until[id] || !(tick + id as u64).is_multiple_of(TICKS_PER_SECOND as u64) {
                continue;
            }
            let toward = match guest {
                // (and once around the guest, they go on as they are: past, and back)
                Some(g) if distance_squared(g.position, me.position) < self.hunt_range * self.hunt_range => {
                    Some(g.position)
                }
                _ => grid
                    .nearest_enemy(contacts, id as u16, &me, self.hunt_range, self.teams)
                    .map(|enemy| contacts[enemy as usize].expect("the grid holds players that are there").position),
            };
            if let Some(to) = toward {
                headings.push((id as u16, self.course(me.position, to)));
            }
        }
        headings
    }

    /// The shots this tick: (shooter, target) pairs. `contacts` is indexed by
    /// player id, `None` for an id not in the match.
    pub fn shots(&mut self, tick: u64, contacts: &[Option<Contact>]) -> Vec<(u16, u16)> {
        let contacts = &*self.without_guest(contacts);
        let grid = Grid::of(contacts, self.range);
        let mut shots = Vec::new();
        for id in 0..self.targets.len().min(contacts.len()) {
            let Some(me) = contacts[id].filter(|c| c.alive) else {
                self.targets[id] = None;
                continue;
            };
            let keep = self.targets[id].filter(|t| self.is_target(&me, contacts.get(*t as usize).copied().flatten()));
            let target = keep.or_else(|| grid.nearest_enemy(contacts, id as u16, &me, self.range, self.teams));
            self.targets[id] = target;
            let Some(target) = target else { continue };
            if tick >= self.next_shot[id] {
                if self.next_shot[id] != 0 {
                    shots.push((id as u16, target));
                }
                // (the first time only sets the clock, so that a crowd does not fire in step)
                let jitter = 0.5 + self.rng.next_f32();
                self.next_shot[id] = tick + (self.mean_gap_ticks * jitter).round().max(1.0) as u64;
            }
        }
        shots
    }

    fn is_target(&self, me: &Contact, other: Option<Contact>) -> bool {
        other.is_some_and(|o| {
            o.alive
                && (!self.teams || o.team != me.team)
                && distance_squared(me.position, o.position) <= self.range * self.range
        })
    }
}

fn distance_squared(a: [f32; 3], b: [f32; 3]) -> f32 {
    (0..3).map(|i| (a[i] - b[i]).powi(2)).sum()
}

/// Players by the square of the map they stand in, `range` wide.
struct Grid {
    cell: f32,
    cells: HashMap<(i32, i32), Vec<u16>>,
}

impl Grid {
    fn of(contacts: &[Option<Contact>], range: f32) -> Grid {
        let cell = range.max(1.0);
        let mut cells: HashMap<(i32, i32), Vec<u16>> = HashMap::new();
        for (id, c) in contacts.iter().enumerate() {
            if let Some(c) = c.filter(|c| c.alive) {
                cells.entry(Grid::key(cell, c.position)).or_default().push(id as u16);
            }
        }
        Grid { cell, cells }
    }

    fn key(cell: f32, p: [f32; 3]) -> (i32, i32) {
        ((p[0] / cell).floor() as i32, (p[1] / cell).floor() as i32)
    }

    fn nearest_enemy(
        &self,
        contacts: &[Option<Contact>],
        me: u16,
        mine: &Contact,
        range: f32,
        teams: bool,
    ) -> Option<u16> {
        let (cx, cy) = Grid::key(self.cell, mine.position);
        let mut best: Option<(f32, u16)> = None;
        for dx in -1..=1 {
            for dy in -1..=1 {
                for &other in self.cells.get(&(cx + dx, cy + dy)).into_iter().flatten() {
                    let Some(o) = contacts[other as usize] else { continue };
                    if other == me || (teams && o.team == mine.team) {
                        continue;
                    }
                    let d2 = distance_squared(mine.position, o.position);
                    if d2 <= range * range && best.is_none_or(|(b, _)| d2 < b) {
                        best = Some((d2, other));
                    }
                }
            }
        }
        best.map(|(_, id)| id)
    }
}

/// How a respawn after a death came about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Path {
    /// At a starting location that was free, the tick the timer ran out.
    FreeStart,
    /// Beside a starting location, the tick the timer ran out.
    BesideStart,
    /// In a wave: the player was made to wait for it.
    Wave,
}

/// A respawn after a death, in seconds.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Respawn {
    /// The respawn timer, with any penalty: from the tick the death was seen to the tick it ran out.
    pub timer: f64,
    /// The time waited after the timer ran out, until the player spawned.
    pub after: f64,
    pub path: Path,
}

/// How long players are out of the world, from the standings the load sees: for a joining
/// player's first spawn (the whole wait), and for each respawn after a death, split into the
/// timer (`Died { respawn_at }`, which the standing keeps as `due_tick`) and the wait after it.
#[derive(Default)]
pub struct Respawns {
    out: HashMap<u16, Out>,
    /// The whole wait of each joining player's first spawn.
    pub first: Vec<f64>,
    pub respawns: Vec<Respawn>,
}

struct Out {
    since: u64,
    /// The tick the timer ran out (the first one seen while dead).
    due: Option<u64>,
    waited: bool,
}

/// What a player's standing says (the `STATE_*` codes: 0 alive, 1 dead, 2 waiting for a wave).
#[derive(Debug, Clone, Copy)]
pub struct Seen {
    pub player: u16,
    pub state: u8,
    pub due_tick: u64,
    pub spawns: u32,
    pub spawned_tick: u64,
    /// Where the player last spawned.
    pub spawn: [f32; 3],
}

impl Respawns {
    /// A player's standing, as of match tick `tick`. `starts` are the map's starting locations
    /// (settled, as the server has them).
    pub fn observe(&mut self, tick: u64, seen: Seen, starts: &[[f32; 3]]) {
        let ticks = |n: u64| n as f64 / TICKS_PER_SECOND as f64;
        if seen.state != 0 {
            let out = self.out.entry(seen.player).or_insert(Out { since: tick, due: None, waited: false });
            match seen.state {
                1 => {
                    out.due.get_or_insert(seen.due_tick);
                }
                _ => out.waited = true,
            }
        } else if let Some(out) = self.out.remove(&seen.player) {
            if seen.spawns <= 1 {
                self.first.push(ticks(seen.spawned_tick.saturating_sub(out.since)));
                return;
            }
            let due = out.due.unwrap_or(out.since).max(out.since);
            let at_a_start =
                starts.iter().any(|s| (s[0] - seen.spawn[0]).abs() < 0.05 && (s[1] - seen.spawn[1]).abs() < 0.05);
            let path = if out.waited {
                Path::Wave
            } else if at_a_start {
                Path::FreeStart
            } else {
                Path::BesideStart
            };
            self.respawns.push(Respawn {
                timer: ticks(due - out.since),
                after: ticks(seen.spawned_tick.saturating_sub(due)),
                path,
            });
        }
    }

    /// The respawns that took `path`.
    pub fn by_path(&self, path: Path) -> usize {
        self.respawns.iter().filter(|r| r.path == path).count()
    }
}

/// Median, p90, p99 and the longest of `values` (sorted by this); `None` for none.
pub fn spread(values: &mut [f64]) -> Option<[f64; 4]> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let at = |q: f64| values[((values.len() - 1) as f64 * q) as usize];
    Some([at(0.5), at(0.9), at(0.99), values[values.len() - 1]])
}

/// How many of the map's Slayer starting locations are free for a player of each team (by the
/// engine's rating: see `halo_sim::spawn::rate`) with `others` in the world. Without teams the
/// two counts are the same.
pub fn free_starts(map: &halo_sim::MapData, teams: bool, others: &[halo_sim::spawn::Occupant]) -> [usize; 2] {
    let mut free = [0usize; 2];
    let game_type = halo_sim::gametype::slayer(teams);
    for start in map.starts.iter().filter(|s| game_type.uses_start(s)) {
        for (team, count) in free.iter_mut().enumerate() {
            if halo_sim::spawn::rate(map, teams, team as u8, &start.position, others) > 0.0 {
                *count += 1;
            }
        }
    }
    free
}

#[cfg(test)]
mod tests {
    use super::*;

    fn standing(state: u8, due_tick: u64, spawns: u32, spawned_tick: u64, spawn: [f32; 3]) -> Seen {
        Seen { player: 1, state, due_tick, spawns, spawned_tick, spawn }
    }

    const STARTS: [[f32; 3]; 1] = [[0.0, 0.0, 0.0]];

    #[test]
    fn a_respawn_is_split_into_the_timer_and_the_wait_after_it_and_told_by_how_it_came_about() {
        let mut r = Respawns::default();
        // dies at tick 100, timer to 250, spawns at once beside the start
        r.observe(100, standing(1, 250, 1, 0, [0.0; 3]), &STARTS);
        r.observe(250, standing(0, 0, 2, 250, [0.6, 0.0, 0.0]), &STARTS);
        // dies at 300, timer to 450, spawns on the start itself
        r.observe(300, standing(1, 450, 2, 250, [0.0; 3]), &STARTS);
        r.observe(450, standing(0, 0, 3, 450, [0.0; 3]), &STARTS);
        // dies at 500, timer to 650, told to wait for the wave at 750
        r.observe(500, standing(1, 650, 3, 450, [0.0; 3]), &STARTS);
        r.observe(650, standing(2, 750, 3, 450, [0.0; 3]), &STARTS);
        r.observe(750, standing(0, 0, 4, 750, [0.6, 0.0, 0.0]), &STARTS);
        let paths: Vec<Path> = r.respawns.iter().map(|x| x.path).collect();
        assert_eq!(paths, [Path::BesideStart, Path::FreeStart, Path::Wave]);
        let secs = |n: f64| n / TICKS_PER_SECOND as f64;
        assert_eq!(r.respawns[0], Respawn { timer: secs(150.0), after: 0.0, path: Path::BesideStart });
        assert_eq!(r.respawns[2].timer, secs(150.0), "the timer is not the wave's wait");
        assert_eq!(r.respawns[2].after, secs(100.0));
        assert_eq!((r.by_path(Path::FreeStart), r.by_path(Path::BesideStart), r.by_path(Path::Wave)), (1, 1, 1));
    }

    #[test]
    fn a_late_respawn_is_only_a_wave_if_the_player_was_seen_waiting_for_one() {
        let mut r = Respawns::default();
        r.observe(100, standing(1, 250, 1, 0, [0.0; 3]), &STARTS);
        r.observe(300, standing(0, 0, 2, 300, [0.6, 0.0, 0.0]), &STARTS);
        assert_eq!(r.respawns[0].path, Path::BesideStart, "not told to wait: something else delayed it");
        assert!(r.respawns[0].after > 1.0);
    }

    #[test]
    fn a_joining_players_first_spawn_is_not_a_respawn() {
        let mut r = Respawns::default();
        r.observe(10, standing(1, 10, 0, 0, [0.0; 3]), &STARTS);
        r.observe(40, standing(0, 0, 1, 40, [0.0; 3]), &STARTS);
        assert!(r.respawns.is_empty());
        assert_eq!(r.first, [30.0 / TICKS_PER_SECOND as f64]);
    }

    #[test]
    fn a_start_with_an_enemy_close_by_is_not_free_for_a_team() {
        use halo_sim::fixtures::{flat_floor_map, start_at, with_starts};
        use halo_sim::spawn::Occupant;
        let map = with_starts(flat_floor_map(), &[start_at(0.0, 0.0, 0), start_at(30.0, 0.0, 0)]);
        assert_eq!(free_starts(&map, true, &[]), [2, 2]);
        // a red player 1.2 from the first start: red may spawn there, blue may not
        let red = Occupant { position: [1.2, 0.0, 0.0], team: 0 };
        assert_eq!(free_starts(&map, true, &[red]), [2, 1]);
        // without teams everyone is an enemy
        assert_eq!(free_starts(&map, false, &[red]), [1, 1]);
    }

    #[test]
    fn spread_gives_the_median_the_tails_and_the_longest() {
        let mut v: Vec<f64> = (1..=100).map(f64::from).collect();
        v.reverse();
        assert_eq!(spread(&mut v), Some([50.0, 90.0, 99.0, 100.0]));
        assert_eq!(spread(&mut []), None);
    }

    fn at(x: f32, team: u8) -> Option<Contact> {
        Some(Contact { position: [x, 0.0, 0.0], team, alive: true })
    }

    /// Every shot of `ticks` ticks.
    fn fire(gunner: &mut Gunner, contacts: &[Option<Contact>], ticks: u64) -> Vec<(u16, u16)> {
        (1..=ticks).flat_map(|tick| gunner.shots(tick, contacts)).collect()
    }

    #[test]
    fn players_shoot_the_nearest_enemy_in_range_and_never_their_team() {
        // 0 and 1 are red, 2 and 3 blue; 3 is nearer to 0 than 2 is, 1 is nearest of all
        let contacts = [at(0.0, 0), at(1.0, 0), at(10.0, 1), at(6.0, 1)];
        let mut gunner = Gunner::new(4, 30.0, 2.0, true, 1);
        let shots = fire(&mut gunner, &contacts, 300);
        assert!(!shots.is_empty());
        for (shooter, target) in &shots {
            let (s, t) = (contacts[*shooter as usize].unwrap(), contacts[*target as usize].unwrap());
            assert_ne!(s.team, t.team, "{shooter} shot {target}, of their own team");
        }
        assert!(shots.iter().filter(|(s, _)| *s == 0).all(|(_, t)| *t == 3), "0 shoots the nearer enemy");
    }

    #[test]
    fn nobody_is_shot_beyond_the_range_or_dead_and_the_dead_do_not_shoot() {
        let mut dead = at(5.0, 1).unwrap();
        dead.alive = false;
        let contacts = [at(0.0, 0), Some(dead), at(100.0, 1)];
        let mut gunner = Gunner::new(3, 30.0, 2.0, true, 2);
        assert!(fire(&mut gunner, &contacts, 300).is_empty());
    }

    #[test]
    fn a_target_is_kept_until_it_dies_and_then_the_next_is_chosen() {
        let mut contacts = vec![at(0.0, 0), at(5.0, 1), at(8.0, 1)];
        let mut gunner = Gunner::new(3, 30.0, 4.0, true, 3);
        let first = fire(&mut gunner, &contacts, 120);
        assert!(first.iter().filter(|(s, _)| *s == 0).all(|(_, t)| *t == 1));
        // (1 moves to where 2 is nearer: still shot at, as it is in range)
        contacts[1] = at(20.0, 1);
        let kept = fire(&mut gunner, &contacts, 120);
        assert!(kept.iter().filter(|(s, _)| *s == 0).all(|(_, t)| *t == 1));
        contacts[1].as_mut().unwrap().alive = false;
        let next = (121..=400).flat_map(|tick| gunner.shots(tick, &contacts)).collect::<Vec<_>>();
        assert!(next.iter().filter(|(s, _)| *s == 0).all(|(_, t)| *t == 2));
        assert!(next.iter().any(|(s, _)| *s == 0));
    }

    #[test]
    fn a_player_fires_about_as_often_as_asked() {
        let contacts = [at(0.0, 0), at(5.0, 1)];
        let mut gunner = Gunner::new(2, 30.0, 3.0, true, 4);
        let seconds = 100;
        let shots = fire(&mut gunner, &contacts, seconds * TICKS_PER_SECOND as u64);
        let rate = shots.iter().filter(|(s, _)| *s == 0).count() as f32 / seconds as f32;
        assert!((2.5..3.5).contains(&rate), "{rate} shots a second");
    }

    #[test]
    fn a_player_with_nobody_in_range_walks_towards_the_nearest_enemy_a_second() {
        // 0 is red, at the origin; 1 is red too, 2 blue 100 units along +y; 3 blue 120 along -x
        let contacts = [
            at(0.0, 0),
            at(5.0, 0),
            Some(Contact { position: [0.0, 100.0, 0.0], ..at(0.0, 1).unwrap() }),
            at(-120.0, 1),
        ];
        let mut gunner = Gunner::new(4, 30.0, 2.0, true, 6).with_hunt_range(150.0);
        assert!(gunner.shots(1, &contacts).is_empty(), "nobody is in range");
        let headings: Vec<(u16, f32)> = (0..30).flat_map(|tick| gunner.hunt(tick, &contacts)).collect();
        let zero: Vec<f32> = headings.iter().filter(|(p, _)| *p == 0).map(|(_, h)| *h).collect();
        assert_eq!(zero.len(), 1, "once a second");
        assert!((zero[0] - core::f32::consts::FRAC_PI_2).abs() < 1e-5, "towards +y: {}", zero[0]);
        // the blue ones walk towards red (the nearer: 0, then 1)
        assert!(headings.iter().any(|(p, h)| *p == 2 && (h + core::f32::consts::FRAC_PI_2).abs() < 0.1));
    }

    #[test]
    fn a_player_who_has_a_target_is_not_sent_hunting() {
        let contacts = [at(0.0, 0), at(5.0, 1)];
        let mut gunner = Gunner::new(2, 30.0, 2.0, true, 7);
        gunner.shots(1, &contacts);
        assert!((0..30).all(|tick| gunner.hunt(tick, &contacts).is_empty()));
    }

    #[test]
    fn a_hunter_who_has_not_got_anywhere_turns_a_new_way_and_is_left_to_walk_it() {
        // 0 walks into a wall: for ten seconds it is where it was, with an enemy straight ahead
        let contacts = [at(0.0, 0), at(100.0, 1)];
        let mut gunner = Gunner::new(2, 30.0, 2.0, true, 8).with_hunt_range(150.0);
        let mut turns = Vec::new();
        let mut hunts = Vec::new();
        for tick in 0..300 {
            for (player, heading) in gunner.hunt(tick, &contacts) {
                if player == 0 {
                    if (heading - 0.0).abs() < 1e-6 {
                        hunts.push(tick)
                    } else {
                        turns.push((tick, heading))
                    }
                }
            }
        }
        assert!(!turns.is_empty(), "it turned away from the wall");
        let (first, _) = turns[0];
        assert!((60..=120).contains(&first), "after a couple of seconds of getting nowhere: tick {first}");
        assert!(!hunts.iter().any(|t| (first..first + 90).contains(t)), "left to walk the new way for a while");
        assert!(hunts.iter().any(|t| *t < first), "and it was walking at the enemy before");
    }

    #[test]
    fn a_hunter_who_stays_stuck_is_still_pointed_at_their_target_between_detours() {
        // 0 never gets anywhere (a wall it stands against): every few seconds it must be pointed
        // at the enemy again, not found stuck again before that can happen
        let contacts = [at(0.0, 0), at(100.0, 1)];
        let mut gunner = Gunner::new(2, 30.0, 2.0, true, 16).with_hunt_range(150.0);
        let mut pointed = Vec::new();
        let mut turned = 0;
        for tick in 0..30 * 60 {
            for (player, heading) in gunner.hunt(tick, &contacts) {
                match (player, heading.abs() < 1e-6) {
                    (0, true) => pointed.push(tick),
                    (0, false) => turned += 1,
                    _ => {}
                }
            }
        }
        assert!(turned >= 5, "it keeps being turned a new way: {turned}");
        let late = pointed.iter().filter(|t| **t > 30 * 30).count();
        assert!(late >= 10, "and is pointed at the enemy again and again, to the end: {late}");
    }

    #[test]
    fn a_hunter_who_is_getting_there_is_not_turned_away() {
        let mut gunner = Gunner::new(2, 30.0, 2.0, true, 9).with_hunt_range(500.0);
        for tick in 0..600 {
            // 0 walks along x at 4 units a second; the enemy is straight ahead
            let walker = Some(Contact { position: [tick as f32 * 0.13, 0.0, 0.0], team: 0, alive: true });
            let contacts = [walker, at(400.0, 1)];
            for (player, heading) in gunner.hunt(tick, &contacts) {
                assert!(player != 0 || heading.abs() < 1e-3, "turned away at tick {tick}: {heading}");
            }
        }
    }

    #[test]
    fn everyone_walks_to_the_guest_and_nobody_shoots_them() {
        // 0 and 2 are red, 1 is blue; 3 is the guest, blue, 80 units from 0
        let guest = Some(Contact { position: [80.0, 0.0, 0.0], ..at(0.0, 1).unwrap() });
        let contacts = [at(0.0, 0), at(300.0, 1), at(0.0, 0), guest];
        let mut gunner = Gunner::new(4, 30.0, 2.0, true, 10).with_hunt_range(500.0).with_guest(3);
        let headings: Vec<(u16, f32)> = (0..30).flat_map(|tick| gunner.hunt(tick, &contacts)).collect();
        assert!(headings.iter().any(|(p, h)| *p == 0 && h.abs() < 1e-5), "0 walks to the guest");
        assert!(
            headings.iter().any(|(p, h)| *p == 1 && (h - core::f32::consts::PI).abs() < 1e-5),
            "1 does, from the other side"
        );
        // standing next to the guest, an enemy of theirs does not shoot
        let near = [at(0.0, 0), Some(Contact { position: [3.0, 0.0, 0.0], ..at(0.0, 1).unwrap() }), None, None];
        let mut gunner = Gunner::new(4, 30.0, 2.0, true, 11).with_guest(0);
        assert!(fire(&mut gunner, &near, 300).is_empty());
    }

    /// A map drawn in characters (see `nav::picture`) and the grid of it.
    fn drawn(rows: &[&str]) -> (crate::nav::picture::Picture, Arc<Nav>) {
        let picture = crate::nav::picture::Picture::of(rows);
        let nav = Arc::new(Nav::build(&picture, 1.0, &[picture.find('A'), picture.find('B')]));
        (picture, nav)
    }

    fn contact(position: [f32; 3], team: u8) -> Option<Contact> {
        Some(Contact { position, team, alive: true })
    }

    #[test]
    fn a_hunter_with_a_wall_in_the_way_is_steered_round_it_and_arrives() {
        let (picture, nav) =
            drawn(&["..........", "....#.....", "....#.....", "A...#....B", "....#.....", "....#....."]);
        let (start, enemy) = (picture.find('A'), picture.find('B'));
        let mut gunner = Gunner::new(2, 5.0, 2.0, true, 12).with_hunt_range(50.0).with_nav(nav);
        // 0 walks the way it is told at 1.5 units a second (and stays where it is if that is
        // into a wall); 1 stands where it is
        let (mut at, mut heading) = (start, 0.0f32);
        let mut blocked = 0;
        let mut arrived = None;
        for tick in 0..30 * 60 {
            let contacts = [contact(at, 0), contact(enemy, 1)];
            for (player, new) in gunner.hunt(tick, &contacts) {
                if player == 0 {
                    heading = new;
                }
            }
            let next = [at[0] + heading.cos() * 0.05, at[1] + heading.sin() * 0.05, 0.0];
            if picture.at(next[0].floor() as i32, next[1].floor() as i32) == '#' {
                blocked += 1;
            } else {
                at = next;
            }
            if (at[0] - enemy[0]).hypot(at[1] - enemy[1]) < 1.5 {
                arrived = Some(tick);
                break;
            }
        }
        assert_eq!(blocked, 0, "never walked into the wall");
        let tick = arrived.expect("the hunter reaches their target");
        assert!(tick < 30 * 20, "and in a reasonable time: {} s", tick / 30);
    }

    #[test]
    fn a_hunter_with_no_path_to_their_target_gets_the_straight_heading() {
        // the wall runs the whole way across: the two are on different stretches of ground
        let (picture, nav) = drawn(&["A..#..B", "...#...", "...#..."]);
        let contacts = [contact(picture.find('A'), 0), contact(picture.find('B'), 1)];
        let mut gunner = Gunner::new(2, 5.0, 2.0, true, 13).with_hunt_range(50.0).with_nav(nav);
        let headings: Vec<(u16, f32)> = (0..30).flat_map(|tick| gunner.hunt(tick, &contacts)).collect();
        let zero: Vec<f32> = headings.iter().filter(|(p, _)| *p == 0).map(|(_, h)| *h).collect();
        assert_eq!(zero, vec![0.0], "as before: straight at the enemy, east");
    }

    #[test]
    fn a_hunter_off_the_grid_gets_the_straight_heading() {
        let (picture, nav) = drawn(&["A.....B"]);
        // 0 stands somewhere the map has no ground, to the north-east of 1 (at the far end)
        let off = [3.5, 40.0, 0.0];
        let contacts = [contact(off, 0), contact(picture.find('B'), 1)];
        let mut gunner = Gunner::new(2, 5.0, 2.0, true, 14).with_hunt_range(100.0).with_nav(nav);
        let headings: Vec<(u16, f32)> = (0..30).flat_map(|tick| gunner.hunt(tick, &contacts)).collect();
        let (to, from) = (picture.find('B'), off);
        let straight = (to[1] - from[1]).atan2(to[0] - from[0]);
        assert!(headings.iter().any(|(p, h)| *p == 0 && (*h - straight).abs() < 1e-6), "{headings:?}");
    }

    #[test]
    fn the_same_seed_gives_the_same_headings_with_a_grid() {
        let rows = ["A.......", ".####...", "...#.##.", ".#...#B."];
        let run = || {
            let (picture, nav) = drawn(&rows);
            let contacts = [contact(picture.find('A'), 0), contact(picture.find('B'), 1)];
            let mut gunner = Gunner::new(2, 3.0, 2.0, true, 15).with_hunt_range(50.0).with_nav(nav);
            (0..300).flat_map(|tick| gunner.hunt(tick, &contacts)).collect::<Vec<_>>()
        };
        assert_eq!(run(), run());
    }

    #[test]
    fn in_a_free_for_all_everyone_is_an_enemy() {
        let contacts = [at(0.0, 0), at(5.0, 0)];
        let mut gunner = Gunner::new(2, 30.0, 2.0, false, 5);
        let shots = fire(&mut gunner, &contacts, 300);
        assert!(shots.contains(&(0, 1)) && shots.contains(&(1, 0)));
    }
}
