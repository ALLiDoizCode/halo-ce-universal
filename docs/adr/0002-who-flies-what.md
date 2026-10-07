# Who flies what in the large-scale mode

In large-scale mode who flies a moving thing is decided by two questions: does the thing outlive its thrower's control, and must the shooter see it register as they saw it? A thing that outlives its thrower's control and that every recipient must see identically is the **server's**. A shot that the shooter must see register as they saw it is the **client's**. A driven vehicle is its driver's movement, which the server validates as it validates walking. A push on a player is the server's to give and the client's to apply.

## The four cases

- **Server-flown: a grenade, an empty vehicle, a flag, the ball.** Once released, nobody steers it, and every recipient has to see the same bounce, the same rest and the same detonation, or two players disagree about whether a flag was dropped on the cliff or in front of it. One simulation, on the server, is the only way to get that. The server also decides the result: a client cannot claim a grenade that did not land.
- **Client-flown: a weapon's projectile, from the hand or from a vehicle.** The shooter fires it and flies it in their own game, so that what they saw is what registered: the rocket that visibly passes a corner is not refused because a copy flown by the server would have bounced elsewhere. The client reports what it hit, and the server checks the report as it checks any hit (the weapon could deal that damage, the target was where the server had it, the origin was reachable since it could have been fired) and deals the damage itself.
- **The driver's movement: a driven vehicle.** The driver's client moves it, at once, from the driver's input, as it moves the driver's own player. The server validates the movement with the same speed bounds and map collision it applies to walking, and refuses what moves faster than the vehicle can or through the map. When the driver leaves, the vehicle becomes an empty vehicle and the server's.
- **Server-given, client-applied: a push on a player.** An explosion pushes the players it reaches. The server decides that a push is due, and how much, from the explosion it has checked; the client applies it to its own player at once. A client that is never given a push has none to apply, and the server refuses a movement that a push it did not give would explain.

## Grenades depart from the letter of #1

#1's Authority section says each client decides its own player's movement and its own hits, and the server simulates everything nobody owns. A thrown grenade is something its thrower owns, so by that letter the thrower's client would fly it, as it flies its own rocket. We chose to fly it on the server because a grenade is the one shot that outlives its thrower's control: it bounces, comes to rest and goes off seconds later, possibly after the thrower has died or left the match. A recipient has to see what the server sees there, and a grenade flown by a client would have to be handed over to the server at some point anyway, with a jump in its path.

### What it costs the thrower

The server aims a grenade from **its view of the thrower**: the thrower's position and view direction as of the newest state the server has, not as they were on the thrower's screen when the button was pressed. At latency the two differ. The thrower sees the grenade leave from where they were a round trip ago, and the grenade can leave a little off the line they aimed on if they were turning or moving. The thrower cannot see the grenade register as they saw it, which is exactly the property a client-flown projectile has and a grenade does not have. We accept this because a grenade is slow and has a timer, so a late and slightly shifted start is a small error against its path, where it would be a large one for a rocket.

## Considered Options

- **The client flies grenades, like its rockets.** Matches #1's letter and gives the thrower an exact throw, but each recipient then sees a grenade that the server has not simulated, and the handover to the server of a thing that lands, rolls and waits for its timer is a second implementation of what the server has to simulate anyway.
- **The client flies everything it fires, grenades and rockets alike, and the server only validates.** Rejected for the same reason: a grenade has to be the same for every recipient for seconds after its thrower stopped controlling it.
- **The server flies every projectile, rockets included.** Every shooter would see their shot register a round trip late, and the server's tick would carry every projectile in a 500-player match. A rocket is fast and short-lived, so the server's simulation buys nothing the hit check does not already give.
- **The server drives vehicles from the driver's input.** The driver's input would take a round trip to move the vehicle they are in, which is not tolerable for driving. Driving is movement, and the server already validates movement.
- **The client applies its own pushes.** A client could then fly by claiming an explosion, which is the exploit the server's giving of the push prevents.

## Consequences

- A thrower at high latency sees grenades start a round trip behind their own view. Where that is bad enough, the fix is to send the thrower's view with the throw and have the server aim from it within the look-back it already keeps for hits, not to move grenades to the client.
- Every grenade, empty vehicle, flag and ball is on the server's tick and is to be sent to recipients within their budget. Grenades are not yet: today every grenade goes to every recipient outside the budget, until #85.
- A new moving thing is placed by the rule above: ask whether it outlives its thrower's control and whether every recipient must see it identically.
