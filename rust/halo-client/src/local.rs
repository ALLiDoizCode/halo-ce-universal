//! The local player's own movement, as the client computes it: the player's
//! controls and the map's collision data go in, where the player is now comes
//! out ([`halo_sim::walk`], the same movement the server's validation is built
//! around). The client shows it at once and reports it; the server only checks
//! the report.
//!
//! The map is the player's own copy of the Xbox map file, which the game has
//! loaded as well, read here for its collision data and its tags' movement
//! values. It is read on a thread of its own, since inflating it takes a
//! moment the game should not wait for: until it is in, [`Local::step`] has
//! nothing to give and the game moves the player as it did before.

use std::sync::{Arc, OnceLock};

use halo_sim::walk::{walk, Body, Controls};
use halo_sim::MapData;

type Loaded = Arc<OnceLock<Result<MapData, String>>>;

/// What one tick of [`Local::step`] came to.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Moved {
    pub position: [f32; 3],
    /// World units a second.
    pub velocity: [f32; 3],
    /// Not standing on anything.
    pub airborne: bool,
}

pub struct Local {
    map: Loaded,
    body: Option<Body>,
}

impl Local {
    /// Start reading the map file at `path` (an Xbox map file, as the game's
    /// own data folder has it).
    pub fn load(path: String) -> Local {
        let map: Loaded = Arc::new(OnceLock::new());
        let loading = map.clone();
        let spawned = std::thread::Builder::new().name("halo-large-map".into()).spawn(move || {
            let result = halo_map::HaloMap::from_path(&path).map(MapData::from).map_err(|e| format!("{path}: {e}"));
            let _ = loading.set(result);
        });
        if let Err(e) = spawned {
            let _ = map.set(Err(format!("a thread to read the map: {e}")));
        }
        Local { map, body: None }
    }

    /// A player on a map that is already in hand (for tests).
    pub fn with_map(map: MapData) -> Local {
        let loaded: Loaded = Arc::new(OnceLock::new());
        let _ = loaded.set(Ok(map));
        Local { map: loaded, body: None }
    }

    /// Why the map could not be read, if it could not.
    pub fn error(&self) -> Option<String> {
        self.map.get().and_then(|r| r.as_ref().err().cloned())
    }

    /// The map is in and movement will be computed.
    pub fn ready(&self) -> bool {
        matches!(self.map.get(), Some(Ok(_)))
    }

    /// Put the player at `position`, at rest: where the server placed them, or
    /// where the engine has the unit until the map is in.
    pub fn place(&mut self, position: [f32; 3]) {
        self.body = Some(Body::at(position));
    }

    /// One tick of the player's controls. `None` while the map is not in or
    /// the player has not been placed.
    pub fn step(&mut self, controls: Controls) -> Option<Moved> {
        let Some(Ok(map)) = self.map.get() else { return None };
        let body = self.body.as_mut()?;
        walk(map, body, &controls);
        Some(Moved { position: body.position, velocity: body.velocity_per_second(), airborne: body.airborne })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use halo_sim::fixtures::flat_floor_map;

    fn forward(yaw: f32) -> Controls {
        Controls { forward: 1.0, strafe: 0.0, yaw, pitch: 0.0 }
    }

    #[test]
    fn there_is_no_movement_until_the_map_is_in_and_the_player_placed() {
        let mut local = Local::with_map(flat_floor_map());
        assert_eq!(local.step(forward(0.0)), None, "not placed yet");
        local.place([0.0, 0.0, 0.0]);
        assert!(local.step(forward(0.0)).is_some());
    }

    #[test]
    fn a_player_walks_with_the_controls_they_give() {
        let mut local = Local::with_map(flat_floor_map());
        local.place([0.0, 0.0, 0.0]);
        let mut last = None;
        for _ in 0..90 {
            last = local.step(forward(0.0));
        }
        let moved = last.unwrap();
        assert!(moved.position[0] > 2.0 && !moved.airborne, "{moved:?}");
        // the velocity is a second's, and is the map's run speed by now
        assert!((moved.velocity[0] - flat_floor_map().movement.run_forward_speed).abs() < 1e-3);
    }

    #[test]
    fn placing_the_player_again_starts_them_over_at_rest() {
        let mut local = Local::with_map(flat_floor_map());
        local.place([0.0, 0.0, 0.0]);
        for _ in 0..60 {
            local.step(forward(0.0));
        }
        local.place([10.0, 5.0, 0.0]);
        let moved = local.step(Controls::standing(0.0)).unwrap();
        assert_eq!((moved.position[0], moved.position[1]), (10.0, 5.0));
        assert_eq!(moved.velocity[0], 0.0);
    }

    #[test]
    fn a_map_file_that_is_not_there_is_an_error_not_a_hang() {
        let local = Local::load("/nonexistent/maps/nothing.map".into());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while local.error().is_none() {
            assert!(std::time::Instant::now() < deadline, "no answer from the loading thread");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(!local.ready());
        assert!(local.error().unwrap().contains("nothing.map"));
    }
}
