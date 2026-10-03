/*
LARGE_EFFECTS.C

What the large-scale mode shows and plays of a fight, on the engine's side:
the part of the mode that is nothing of the library's (large_mode.c is the
adapter, which knows it), so it is in every build that has the engine.

The mode has no engine damage for a player's unit (the server deals it, see
large_mode_damage_deals), and the effects of fighting come in two kinds:

- the effects of a weapon: a unit's weapon is fired by the engine from the
  controls it is given, as it is for the local player and, here, for every
  remote player whose weapon the server says fired (large_mode.c reads their
  shots from the state the gateway sends). The muzzle flash, the projectile and
  everywhere it lands, the shell, the sounds and the unit's firing animation are
  all the weapon's own tags', so a weapon added to the game by its data has them.
  large_effects_give_weapon puts a weapon in a unit's hand, and
  large_effects_refill_weapon keeps a remote player's from running dry, which
  only the server's player knows the truth of;
- the effects of being hurt, which come from damage that the engine did not
  deal, only the server: when its table says a player was hurt (hit count up)
  large_effects_hurt does what the engine does after damage it has dealt (the
  shield's flash is the vitals', large_mode.c's): the unit's pain sound and
  flinch (unit_damage_aftermath), and for the local player the flash of the
  screen, its shake, the damage sound and the direction the hit came from
  (player_effect_start), all from the damage effect tag of the shooter's weapon's
  projectile. large_effects_hit_material gives a projectile that hits a player's
  unit the material that the engine's damage would have, so that the impact
  shows a shield's or a body's. large_effects_kill is the killing blow, with the
  weapon's damage in it, so that the unit dies the way a shot player does and
  says what that sounds like.

large.log_sounds (HALO_LARGE_LOG_SOUNDS) logs every sound the engine is asked to
start (large_effects_sound_requested, from the sound manager), with the game
tick, which is how the automated tests see what played: a sound's tag and the
thing that asked for it.
*/

#include "cseries.h"
#include "effects/player_effects.h"
#include "game/game.h"
#include "game/game_globals.h"
#include "game/players.h"
#include "items/projectile_definitions.h"
#include "items/weapon_definitions.h"
#include "items/weapons.h"
#include "objects/damage.h"
#include "objects/damage_effect_definitions.h"
#include "objects/object_definitions.h"
#include "objects/objects.h"
#include "physics/collision_model_definitions.h"
#include "sound/sound_definitions.h"
#include "tag_files/tag_groups.h"
#include "units/bipeds.h"
#include "units/units.h"

#include <math.h>
#include <string.h>

/* the platform layer's (port/linux/src/port_config.c) */
int config_boolean(char const *name);
void platform_log(char const *format, ...);

static struct
{
	boolean checked;
	boolean log_sounds;
	long hurts;
	long kills;
	long recoils;
} effects;

/* ---------- weapons */

/* a weapon of this definition in the unit's hand; the weapon's object, or NONE if the engine would not
give it to the unit (the game engine's say, or a unit that cannot carry it) */
long large_effects_give_weapon(
	long unit_index,
	long definition_index)
{
	struct object_placement_data placement;
	long weapon_index;

	object_placement_data_new(&placement, definition_index, unit_index);
	weapon_index = object_new(&placement);
	if (weapon_index == NONE)
		return NONE;
	if (!unit_add_weapon_to_inventory(unit_index, weapon_index, _unit_add_weapon_starting))
	{
		object_delete(weapon_index);
		return NONE;
	}
	return weapon_index;
}

/* a remote player's weapon, which the server's player knows the rounds of and the engine does not: it has a
round to fire and no heat, so that every shot the player is said to fire is fired (and a reload, which is
asked for, has a magazine that is not full to reload). A magazine that is reloading is left to finish. */
void large_effects_refill_weapon(
	long weapon_index)
{
	struct weapon_datum *weapon = weapon_try_and_get(weapon_index);
	struct weapon_definition const *definition;
	short magazine_index;

	if (!weapon)
		return;
	definition = weapon_definition_get(weapon->definition_index);
	for (magazine_index = 0; magazine_index < definition->weapon.magazines.count && magazine_index < 2; magazine_index++)
	{
		struct weapon_magazine_definition const *magazine_definition = TAG_BLOCK_GET_ELEMENT(
			&definition->weapon.magazines, magazine_index, struct weapon_magazine_definition);
		struct weapon_magazine *magazine = &weapon->weapon.magazines[magazine_index];

		if (magazine->state == 0)
		{
			magazine->rounds_loaded = MAX(magazine_definition->rounds_loaded_maximum - 1, 1);
			magazine->rounds_total = magazine_definition->rounds_total_maximum;
		}
	}
	weapon->weapon.heat = 0.0f;
	weapon->weapon.age = 0.0f;
	/* (the weapon of a player who comes into range is not one that was only just drawn: there is no
	first-person animation to wait for, so it is ready at once) */
	if (weapon->weapon.state == _weapon_state_ready)
		weapon->weapon.state_timer = 0;
}

/* ---------- the shooter's weapon's damage */

/* the damage effect tag of what a weapon's first trigger fires, or NONE */
static long large_effects_damage_effect(
	long weapon_definition_index)
{
	struct weapon_definition const *definition;
	struct weapon_trigger_definition const *trigger;

	if (weapon_definition_index == NONE)
		return NONE;
	definition = weapon_definition_get(weapon_definition_index);
	if (definition->weapon.triggers.count <= 0)
		return NONE;
	trigger = TAG_BLOCK_GET_ELEMENT(&definition->weapon.triggers, 0, struct weapon_trigger_definition);
	if (trigger->projectile.index == NONE)
		return NONE;
	return projectile_definition_get(trigger->projectile.index)->projectile.impact_damage.index;
}

/* the damage of a shot from `shooter_index` (a unit, or NONE if it is not in the world) with the weapon at
the unit `victim_index`: the weapon's damage effect, scaled for a full shot, coming from where the shooter is
*/
static boolean large_effects_damage(
	struct damage_data *damage,
	long victim_index,
	long shooter_index,
	long weapon_definition_index)
{
	long effect_index = large_effects_damage_effect(weapon_definition_index);
	real_point3d victim_position;
	real length;

	if (effect_index == NONE)
		return FALSE;
	damage_data_new(damage, effect_index);
	SET_FLAG(damage->flags, _damage_from_weapon_bit, TRUE);
	damage->scale = 1.0f;
	damage->owner_object_index = shooter_index;
	object_get_origin(victim_index, &victim_position);
	damage->origin = victim_position;
	damage->epicenter = victim_position;
	/* (the way it was going: from the shooter to the victim, level when the shooter is not here) */
	damage->direction.i = 0.0f;
	damage->direction.j = 1.0f;
	damage->direction.k = 0.0f;
	if (shooter_index != NONE && object_try_and_get(shooter_index))
	{
		real_point3d shooter_position;

		object_get_origin(shooter_index, &shooter_position);
		damage->direction.i = victim_position.x - shooter_position.x;
		damage->direction.j = victim_position.y - shooter_position.y;
		damage->direction.k = victim_position.z - shooter_position.z;
		length = (real)sqrt(damage->direction.i * damage->direction.i + damage->direction.j * damage->direction.j +
			damage->direction.k * damage->direction.k);
		if (length > 0.0001f)
		{
			damage->direction.i /= length;
			damage->direction.j /= length;
			damage->direction.k /= length;
		}
		else
		{
			damage->direction.j = 1.0f;
		}
	}
	return TRUE;
}

/* ---------- being hurt */

/* the server says the unit was hurt (it took `shield_lost` of its shield and `body_lost` of its health, as
fractions of full ones; the shield went down if `shield_down`) by a shot of `weapon_definition_index` from
`shooter_index`. What the engine does after damage it has dealt, but for the harm: the unit's pain sound and
flinch, and for the local player (`local_player_index`, or NONE) the screen's flash and shake, the damage
effect's sound and the direction the hit came from. */
void large_effects_hurt(
	long unit_index,
	long local_player_index,
	long shooter_index,
	long weapon_definition_index,
	real shield_lost,
	real body_lost,
	boolean shield_down)
{
	struct damage_data damage;
	unsigned long flags = 0;
	struct damage_effect_definition const *effect;

	if (!large_effects_damage(&damage, unit_index, shooter_index, weapon_definition_index))
		return;
	effects.hurts++;
	effect = damage_effect_definition_get(damage.definition_index);
	if (shield_down)
		SET_FLAG(flags, _object_being_damaged_shield_depleted_bit, TRUE);
	if (local_player_index != NONE)
	{
		/* (the damage's size is what the indicator asks for: more than nothing) */
		player_effect_start(local_player_index, &damage, &damage.direction, damage.scale,
			MAX(effect->damage.damage_upper_bound, 0.01f));
	}
	if (unit_try_and_get(unit_index))
	{
		unit_damage_aftermath(unit_index, &damage, flags, MAX(shield_lost, 0.0f), MAX(body_lost, 0.0f), 1.0f, NONE);
	}
}

/* the killing blow the server says a hit dealt: the unit is killed as a shot player is, by the weapon's
damage effect from the shooter (the sound of the death, the effects and the way the body falls are the damage
effect's); with no weapon to say what killed it, the unit just dies (unit_kill) */
void large_effects_kill(
	long unit_index,
	long shooter_index,
	long weapon_definition_index)
{
	struct damage_data damage;

	effects.kills++;
	if (!large_effects_damage(&damage, unit_index, shooter_index, weapon_definition_index))
	{
		unit_kill(unit_index);
		return;
	}
	/* (the server counts the kill: not the engine, which has the damage's owner as no player) */
	SET_FLAG(damage.flags, _damage_kill_instantly_bit, TRUE);
	object_cause_damage(&damage, unit_index, NONE, NONE, NONE, NULL);
}

/* ---------- the weapon's own effect on the player who fires it */

/* damage the engine does a unit that is a weapon's own recoil at the player firing it (weapons.c: no one's,
from the weapon, at a point, the unit's centre), which shakes the player's screen as a hit does: shown, as
the engine shows it, though the unit takes no harm of it. FALSE for any other damage. */
boolean large_effects_recoil(
	struct damage_data *damage,
	long unit_index)
{
	struct unit_datum *unit = (struct unit_datum *)object_try_and_get_and_verify_type(unit_index, _object_mask_unit);

	if (!unit || unit->unit.player_index == NONE || damage->owner_player_index != NONE ||
		damage->owner_object_index != NONE || !TEST_FLAG(damage->flags, _damage_from_weapon_bit) ||
		TEST_FLAG(damage->flags, _damage_area_of_effect_bit) || damage->origin.x != damage->epicenter.x ||
		damage->origin.y != damage->epicenter.y || damage->origin.z != damage->epicenter.z ||
		damage->epicenter.x != unit->object.bounding_sphere_center.x ||
		damage->epicenter.y != unit->object.bounding_sphere_center.y ||
		damage->epicenter.z != unit->object.bounding_sphere_center.z)
	{
		return FALSE;
	}
	player_effect_start(unit->unit.player_index, damage, &damage->direction, damage->scale, 0.0f);
	effects.recoils++;
	return TRUE;
}

/* ---------- a projectile that hits a player's unit */

/* what the engine's damage to a unit would have said of what was hit, which its projectile's impact
effect reads (projectiles.c, after object_cause_damage): the shield's material while the shield holds,
and the body's part that was hit otherwise, with how much is left of it */
void large_effects_hit_material(
	struct damage_data *damage,
	long object_index,
	short material_index)
{
	struct object_datum *object = object_get(object_index);
	struct object_definition const *definition = object_definition_get(object->definition_index);
	struct collision_model const *model;

	if (definition->object.collision_model.index == NONE)
		return;
	model = collision_model_definition_get(definition->object.collision_model.index);
	if (object->object.maximum_shield_vitality > 0.0f && object->object.shield_vitality > 0.0f)
	{
		damage->material_type = model->resistance.shield_material_type;
		damage->material_effect_scale = object->object.shield_vitality;
	}
	else if (material_index >= 0 && material_index < model->resistance.materials.count)
	{
		damage->material_type = TAG_BLOCK_GET_ELEMENT(&model->resistance.materials, material_index,
			struct damage_resistance_material)->material_type;
		damage->material_effect_scale = PIN(object->object.body_vitality, 0.0f, 1.0f);
	}
}

/* ---------- sounds */

/* the sound manager is asked to start a sound (sound_new_impulse), whether or not anyone is near enough to
hear it or the machine has a sound device: with large.log_sounds its tag is logged, with the game tick */
void large_effects_sound_requested(
	long definition_index,
	long source_identifier)
{
	if (!effects.checked)
	{
		effects.checked = TRUE;
		effects.log_sounds = config_boolean("large.log_sounds") != 0;
	}
	if (!effects.log_sounds || definition_index == NONE)
		return;
	platform_log("large mode: sound %s class %d tick %ld source %ld", tag_get_name(definition_index),
		(int)sound_definition_get(definition_index)->sound_class, game_time_get(), source_identifier);
}
