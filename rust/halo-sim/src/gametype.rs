//! The seam between the rules and a game type. [`crate::rules`] keeps what every
//! game has (who is alive, the respawn timer, waves, the clock and the end);
//! what only a game type knows goes behind [`GameType`]: its name, whether it
//! has teams, which of the map's starting locations it plays on, and what a
//! death is worth, which is the score a scoreboard row carries.
//!
//! Slayer and Team Slayer are the two there are. A game type to come adds one
//! `impl` and a line to [`by_name`] and [`Rules::game_type`](crate::rules::Rules::game_type).

use crate::rules::DeathKind;
use crate::spawn::Start;

/// Who a score goes to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScoreTo {
    /// The player who got the credit for a death.
    Killer,
    /// The player who died.
    Victim,
}

/// What a game type decides.
pub trait GameType: Sync {
    /// The name a server's configuration and the server list use.
    fn name(&self) -> &'static str;

    /// Whether players are on teams (two) and a team's score is the one that counts.
    fn teams(&self) -> bool;

    /// Whether players spawn at this starting location: the game type's own list.
    fn uses_start(&self, start: &Start) -> bool;

    /// What the score is counted in, for a scoreboard's column.
    fn score_unit(&self) -> &'static str;

    /// What a death adds to a score, and whose: the score of the scoreboard row.
    /// `None` when it is worth nothing to anyone.
    fn death_score(&self, kind: DeathKind) -> Option<(ScoreTo, i32)>;
}

/// Slayer, and Team Slayer, which is Slayer with teams: a kill is a point.
#[derive(Debug, Clone, Copy)]
pub struct Slayer {
    pub teams: bool,
}

pub static SLAYER: Slayer = Slayer { teams: false };
pub static TEAM_SLAYER: Slayer = Slayer { teams: true };

impl GameType for Slayer {
    fn name(&self) -> &'static str {
        if self.teams {
            "team_slayer"
        } else {
            "slayer"
        }
    }

    fn teams(&self) -> bool {
        self.teams
    }

    fn uses_start(&self, start: &Start) -> bool {
        start.is_for_slayer()
    }

    fn score_unit(&self) -> &'static str {
        "kills"
    }

    fn death_score(&self, kind: DeathKind) -> Option<(ScoreTo, i32)> {
        match kind {
            DeathKind::Kill => Some((ScoreTo::Killer, 1)),
            DeathKind::Betrayal => Some((ScoreTo::Killer, -1)),
            DeathKind::Suicide => Some((ScoreTo::Victim, -1)),
            DeathKind::Unclaimed => None,
        }
    }
}

/// The game type a server's configuration names.
pub fn by_name(name: &str) -> Option<&'static dyn GameType> {
    [&SLAYER, &TEAM_SLAYER].into_iter().find(|g| g.name() == name).map(|g| g as &dyn GameType)
}

#[cfg(test)]
mod tests {
    use super::*;
    use halo_map::game_type;

    fn start(types: [i16; 4]) -> Start {
        Start { position: [0.0; 3], yaw: 0.0, team: 0, game_types: types }
    }

    #[test]
    fn the_game_types_are_found_by_the_name_a_server_list_shows() {
        for name in ["slayer", "team_slayer"] {
            assert_eq!(by_name(name).unwrap().name(), name);
        }
        assert!(by_name("ctf").is_none());
        assert!(!by_name("slayer").unwrap().teams() && by_name("team_slayer").unwrap().teams());
    }

    #[test]
    fn slayer_plays_on_the_starts_that_list_it_and_scores_kills() {
        let g = by_name("slayer").unwrap();
        assert!(g.uses_start(&start([game_type::SLAYER, 0, 0, 0])));
        assert!(!g.uses_start(&start([game_type::CTF, 0, 0, 0])));
        assert_eq!(g.death_score(DeathKind::Kill), Some((ScoreTo::Killer, 1)));
        assert_eq!(g.death_score(DeathKind::Betrayal), Some((ScoreTo::Killer, -1)));
        assert_eq!(g.death_score(DeathKind::Suicide), Some((ScoreTo::Victim, -1)));
        assert_eq!(g.death_score(DeathKind::Unclaimed), None);
    }
}
