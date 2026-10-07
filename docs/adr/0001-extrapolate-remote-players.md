# Remote players are extrapolated, not interpolated

In large-scale mode a recipient is sent most players less often than every tick, so the client has to place them on the ticks in between. We extrapolate: the drawn position is the held state carried forward by its velocity (with gravity when airborne) to the newest tick the client has heard of, for up to the staleness cap, and the error is faded out when the next state arrives. We chose this over interpolating between the last two states because interpolation draws every player an update interval late (67 to 500 ms outside the near band), and the server's hit check assumes the position a shooter reports is where the target was at about the newest tick the shooter had heard.

## Considered Options

- **Interpolate between the last two states**: never wrong about the path, but always late by an update interval, which puts the drawn position further from what the hit check accepts.
- **Extrapolate near players, interpolate far ones**: two methods to test, for a gain that is hardly visible at the distances where it would apply.

## Consequences

- A player who stops or turns just after a state is drawn ahead of where they are until the next state, by up to twice their speed times the update interval.
- The hit report and the server's look-back were left as they are. If hits are refused because the held state was old, the fix is to send the held state's tick in the report, not to change how players are drawn.
