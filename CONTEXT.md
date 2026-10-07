# Halo CE Universal

A port of Halo CE multiplayer, with a large-scale mode beside the original game: about 500 players in one match on a dedicated server.

## Language

### Player updates in large-scale mode

**Recipient**:
The player a set of updates is chosen for and sent to.
_Avoid_: Observer, viewer, client

**Budget**:
The most bytes a second a recipient is sent, headers included. Fixed per server by its operator; 90 KB/s by default.
_Avoid_: Bandwidth cap, rate limit

**Near player**:
A player within 10 world units of the recipient.
_Avoid_: Nearby player, close player

**Near share**:
The part of a tick's updates that near players may take ahead of everyone else. Near players beyond it take turns.
_Avoid_: Near cap, near quota

**Overdue player**:
A player whose state the recipient has not been sent for the staleness cap (15 ticks).
_Avoid_: Stale player, urgent player

**Staleness bound**:
The stated limit on how old any player's state may be for any recipient: 40 ticks.
_Avoid_: Staleness cap (the 15-tick trigger that makes a player overdue)

**Held state**:
The newest state of a player that the recipient's client has received.
_Avoid_: Last state, latest snapshot

**Drawn position**:
Where the recipient's client shows another player on a given tick.
_Avoid_: Rendered position, shown position

**Drawn error**:
The distance between a player's drawn position and where the server has that player on the same tick.
_Avoid_: Lag, position error

**Band**:
A range of distance from the recipient over which update rates are reported: under 10, 10 to 25, 25 to 60, and over 60 world units.
_Avoid_: Tier, ring
