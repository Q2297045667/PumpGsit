//! Real server-side vehicles for block seats and consenting player stacks.
use crate::{
    bindings::pumpkin::plugin::{
        common::{BlockPos, GameMode, Hand, Position},
        entity_types::EntityType,
        player::Player,
        server::Server,
        world::{self, BlockDirection, BlockState, World},
    },
    config,
    config::MaterialRule,
    enabled, pose,
    state::State,
    storage::Preference,
};
use std::{
    collections::{HashMap, HashSet},
    sync::LazyLock,
};

const SEAT_BASE_HEIGHT: f64 = 0.025;
const TAG: &str = "pumpgsit.seat.v1";
const LEGACY_TAG: &str = "gsit.pumpkin.seat.v1";
pub type Key = (i32, i32, i32);

#[derive(Clone, Copy, PartialEq)]
pub enum Activity {
    Sit,
    PlayerSit,
    Lay,
    LegsUp,
    BellyFlop,
    Spin,
}

impl Activity {
    /// Identifies the four stationary actions which display an animated player model.
    pub fn is_pose(self) -> bool {
        !matches!(self, Self::Sit | Self::PlayerSit)
    }
}

#[derive(Clone)]
struct Seat {
    rider: u32,
    rider_uuid: crate::bindings::pumpkin::plugin::uuid::Uuid,
    vehicle: u32,
    world: String,
    block: Option<Key>,
    had_support: bool,
    original: Position,
    surface: Position,
    vehicle_y: f64,
    activity: Activity,
}

type Riders<T> = LazyLock<State<HashMap<u32, T>>>;
static SEATS: Riders<Seat> = LazyLock::new(State::default);
// A movement callback is issued only after Pumpkin accepted the dismount acknowledgement.
static RETURNS: Riders<(String, Position, bool)> = LazyLock::new(State::default);
/// GSit tracks ordinary seats separately from its four stationary poses on a block.
#[derive(Clone, Copy, PartialEq)]
enum BlockUse {
    Sit(Key),
    Pose(Key),
}

static PENDING: Riders<(String, Option<BlockUse>)> = LazyLock::new(State::default);

/// Holds an admission until every awaited spawn/mount callback has returned.
struct Admission(u32);
impl Drop for Admission {
    /// Releases a pending player and block on success, refusal, or early return.
    fn drop(&mut self) {
        PENDING.with_borrow_mut(|set| set.remove(&self.0));
    }
}

/// Reserves the player and optionally the block to prevent concurrent successful mounts.
fn reserve(id: u32, world: &str, block: Option<BlockUse>) -> Result<Admission, &'static str> {
    let exclusive = block.is_some() && !config().same_block_rest;
    PENDING.with_borrow_mut(|map| {
        if map.contains_key(&id)
            || (exclusive && map.values().any(|(w, b)| w == world && *b == block))
        {
            return Err("occupied");
        }
        map.insert(id, (world.to_owned(), block));
        Ok(Admission(id))
    })
}

/// Returns the plugin-owned action for a rider, if one exists.
pub fn activity(player: &Player) -> Option<Activity> {
    activity_by_id(player.as_entity().get_id())
}

/// Looks up a tracked activity directly from a host event's entity id.
pub fn activity_by_id(id: u32) -> Option<Activity> {
    SEATS.with_borrow(|m| m.get(&id).map(|s| s.activity))
}

/// Applies the command blacklist to seated players and either end of a player stack.
pub fn uses_command_blacklist(player: &Player) -> bool {
    let id = player.as_entity().get_id();
    SEATS.with_borrow(|seats| {
        seats.contains_key(&id)
            || seats
                .values()
                .any(|seat| seat.activity == Activity::PlayerSit && seat.vehicle == id)
    })
}

/// Returns whether another plugin or player has cancelled or replaced this ride.
fn mounted(player: &Player, vehicle: u32) -> bool {
    player
        .as_entity()
        .get_vehicle()
        .is_some_and(|e| e.get_id() == vehicle)
}

/// Matches the material or tag and resolves omitted block properties to their vanilla defaults.
fn matches(rule: &MaterialRule, state: &BlockState) -> bool {
    let name = &rule.name;
    let matches_material = if name.starts_with('#') {
        world::is_block_in_tag(&state.block_name, name)
    } else {
        name.trim_start_matches("minecraft:") == state.block_name.trim_start_matches("minecraft:")
    };
    matches_material
        && rule.properties.as_ref().is_none_or(|properties| {
            // The host resolver ignores unknown properties. Reject them, as Bukkit does.
            properties.iter().all(|p| state.properties.contains(p))
                && world::resolve_block_state(&state.block_name, properties) == Some(state.id)
        })
}

/// Checks world lists by configured world name or Minecraft dimension key.
pub async fn allowed(player: &Player) -> bool {
    let cfg = config();
    if cfg.world_whitelist.is_empty() && cfg.world_blacklist.is_empty() {
        return true;
    }
    let world = player.get_world();
    let names = [world.get_name(), world.get_id(), world.get_dimension()];
    let listed = |entries: &[String]| {
        entries
            .iter()
            .any(|s| names.iter().any(|n| n.eq_ignore_ascii_case(s)))
    };
    (!listed(&cfg.world_blacklist)
        && (cfg.world_whitelist.is_empty() || listed(&cfg.world_whitelist)))
        || player.has_permission("PumpGsit:bypass.world".into()).await
}

/// Selects the support block the same sixteenth of a block below the feet as Bukkit.
fn supporting_block(world: &World, (x, y, z): Position) -> BlockPos {
    let mut pos = BlockPos {
        x: x.floor() as i32,
        y: y.floor() as i32,
        z: z.floor() as i32,
    };
    if world.get_block_state(pos).collision_shapes.is_empty() {
        pos.y = (y - 0.0625).floor() as i32
    }
    pos
}

/// Determines the clicked top face and exact location from the current look ray.
pub fn click_target(player: &Player, pos: BlockPos) -> Option<Position> {
    let reach = player.as_entity().as_living()?.get_attribute_value(
        crate::bindings::pumpkin::plugin::attributes::Attribute::BlockInteractionRange,
    );
    if !reach.is_finite() || reach <= 0.0 {
        return None;
    }
    let hit = player.ray_trace_block(reach, false)?;
    (matches!(hit.face, BlockDirection::Up)
        && (hit.pos.x, hit.pos.y, hit.pos.z) == (pos.x, pos.y, pos.z))
        .then_some(hit.hit_pos)
}

/// Rotates the seat toward the open side of a bottom stair, including inside/outside corners.
fn stair_offset(props: &[(String, String)]) -> (f64, f64, f32) {
    let get = |k: &str| {
        props
            .iter()
            .find(|(name, _)| name == k)
            .map(|(_, v)| v.as_str())
            .unwrap_or("")
    };
    let (mut x, mut z) = match get("facing") {
        "north" => (0.0, 1.0),
        "south" => (0.0, -1.0),
        "east" => (-1.0, 0.0),
        _ => (1.0, 0.0),
    };
    match get("shape") {
        "inner_left" | "outer_left" => (x, z) = (x + z, z - x),
        "inner_right" | "outer_right" => (x, z) = (x - z, z + x),
        _ => {}
    }
    (x * 0.123, z * 0.123, (-x).atan2(z).to_degrees() as f32)
}

/// Checks collisions, material predicates, occupancy and host confirmation before recording a seat.
pub async fn start(
    player: &Player,
    server: &Server,
    kind: Activity,
    click: Option<(BlockPos, Position)>,
) -> Result<Position, &'static str> {
    let cfg = config();
    let rider = player.as_entity();
    if !rider.is_valid()
        || player.is_sneaking()
        || rider.get_vehicle().is_some()
        || player.is_sleeping()
        || crate::crawl::active(player)
        || (click.is_none() && !rider.is_on_ground())
    {
        return Err("state");
    }
    if !allowed(player).await {
        return Err("world");
    }
    let world = player.get_world();
    let original = rider.get_position();
    let pos = click
        .map(|c| c.0)
        .unwrap_or_else(|| supporting_block(&world, original));
    let key = (pos.x, pos.y, pos.z);
    let state = world.get_block_state(pos);
    let props = &state.properties;
    let selected = cfg
        .sit
        .materials
        .iter()
        .filter(|rule| matches(rule, &state))
        // GSit looks up exact BlockData before Material. The last assignment in each map wins.
        .max_by_key(|rule| rule.properties.is_some());
    if click.is_some()
        && (selected.is_none()
            || (cfg.sit.empty_hand_only && player.get_item_in_hand(Hand::Right).is_some()))
    {
        return Err("location");
    }
    if cfg.material_blacklist.iter().any(|r| matches(r, &state)) {
        return Err("location");
    }
    let center = (
        f64::from(pos.x) + 0.5,
        f64::from(pos.y) + 0.5,
        f64::from(pos.z) + 0.5,
    );
    if click.is_some() && too_far(original, center, cfg.sit.max_distance) {
        return Err("location");
    }
    let height = state
        .collision_shapes
        .iter()
        .map(|s| s.max.1)
        .fold(0.0, f64::max);
    if !cfg.allow_unsafe
        && (!world
            .get_block_state(BlockPos {
                y: pos.y + 1,
                ..pos
            })
            .collision_shapes
            .is_empty()
            || height > 1.25
            || (height == 0.0 && cfg.center_block))
    {
        return Err("location");
    }
    let prop = |k: &str, v: &str| props.iter().any(|(a, b)| a == k && b == v);
    let stairs = state.block_name.ends_with("_stairs");
    if click.is_some()
        && cfg.sit.bottom_part_only
        && ((stairs && prop("half", "top"))
            || (state.block_name.ends_with("_slab") && !prop("type", "bottom")))
    {
        return Err("location");
    }
    let (mut surface, mut yaw) = if cfg.center_block {
        (
            (
                center.0,
                f64::from(pos.y) + if height == 0.0 { 1.0 } else { height },
                center.2,
            ),
            rider.get_yaw(),
        )
    } else {
        (click.map(|c| c.1).unwrap_or(original), rider.get_yaw())
    };
    if stairs && prop("half", "bottom") && kind == Activity::Sit {
        let (x, z, angle) = stair_offset(props);
        surface = (center.0 + x, f64::from(pos.y) + 0.5, center.2 + z);
        yaw = angle;
    }
    if state.block_name.trim_start_matches("minecraft:") == "shelf_mushroom"
        && kind == Activity::Sit
    {
        let (x, z, angle) = if prop("facing", "north") {
            (0., 0.2, 180.)
        } else if prop("facing", "south") {
            (0., -0.2, 0.)
        } else if prop("facing", "east") {
            (-0.2, 0., -90.)
        } else {
            (0.2, 0., 90.)
        };
        surface = (center.0 + x, f64::from(pos.y) + height, center.2 + z);
        yaw = angle;
    }
    let height_offset = selected.map_or(0.0, |rule| rule.offset);
    let resting_surface = (surface.0, surface.1 + height_offset, surface.2);
    let vehicle_y = resting_surface.1 + SEAT_BASE_HEIGHT;
    let name = world.get_id();
    let block_use = if kind.is_pose() {
        BlockUse::Pose(key)
    } else {
        BlockUse::Sit(key)
    };
    let _guard = reserve(rider.get_id(), &name, Some(block_use))?;
    forget_return(rider.get_id());
    if !cfg.same_block_rest {
        let occupied = SEATS.with_borrow(|m| {
            m.values()
                .filter(|s| {
                    s.block == Some(key)
                        && s.world == name
                        && s.activity.is_pose() == kind.is_pose()
                })
                .cloned()
                .collect::<Vec<_>>()
        });
        for other in occupied {
            let node = if other.activity == Activity::Sit {
                "PumpGsit:kick.sit"
            } else {
                "PumpGsit:kick.pose"
            };
            if !player.has_permission(node.into()).await {
                return Err("occupied");
            }
            if let Some(victim) = player_by_id(server, other.rider) {
                stop(&victim, server, true).await;
            }
            if SEATS.with_borrow(|m| m.contains_key(&other.rider)) {
                return Err("occupied");
            }
        }
    }
    // Empty block displays have a zero-height passenger attachment and render no block.
    let vehicle = world
        .spawn_entity(
            EntityType::BlockDisplay,
            (resting_surface.0, vehicle_y, resting_surface.2),
        )
        .await;
    if world.get_entity_by_uuid(vehicle.get_uuid()).is_none() {
        return Err("refused");
    }
    let id = vehicle.get_id();
    vehicle.set_has_gravity(false);
    vehicle.set_invulnerable(true);
    vehicle.set_silent(true);
    vehicle.set_rotation(yaw, 0.0);
    if !rider.is_valid() || rider.get_vehicle().is_some() || player.get_world().get_id() != name {
        vehicle.remove();
        return Err("state");
    }
    vehicle.add_passenger(player.as_entity()).await;
    if !mounted(player, id) {
        vehicle.remove();
        return Err("refused");
    }
    vehicle.add_scoreboard_tag(TAG);
    // The bottom step is suitable for the model, but a standing player needs full headroom.
    if kind == Activity::Sit && stairs && prop("half", "bottom") {
        surface.1 = f64::from(pos.y) + height;
    }
    let seat = Seat {
        rider: rider.get_id(),
        rider_uuid: player.get_id(),
        vehicle: id,
        world: name,
        block: Some(key),
        had_support: !state.is_air,
        original,
        surface,
        vehicle_y,
        activity: kind,
    };
    SEATS.with_borrow_mut(|m| m.insert(seat.rider, seat));
    Ok(resting_surface)
}

/// Checks the optional Euclidean distance limit in blocks.
fn too_far(a: Position, b: Position, max: f64) -> bool {
    max > 0.0 && (a.0 - b.0).powi(2) + (a.1 - b.1).powi(2) + (a.2 - b.2).powi(2) > max * max
}

/// Finds an online player by the entity id used in the host's ride and damage events.
pub fn player_by_id(server: &Server, id: u32) -> Option<Player> {
    server
        .get_all_players()
        .into_iter()
        .find(|p| p.as_entity().get_id() == id)
}

/// Resolves the full player stack, rejects cycles, and mounts the consenting top player.
pub async fn stack(player: &Player, target_id: u32, server: &Server) -> bool {
    let cfg = config();
    let options = &cfg.player_sit;
    let rider = player.as_entity();
    if !options.allow_sit
        || !rider.is_valid()
        || player.is_sneaking()
        || player.is_sleeping()
        || matches!(player.get_gamemode(), GameMode::Spectator)
        || rider.get_vehicle().is_some()
        || crate::crawl::active(player)
        || (options.empty_hand_only && player.get_item_in_hand(Hand::Right).is_some())
        || !player.has_permission("PumpGsit:playersit".into()).await
        || !allowed(player).await
    {
        return false;
    }
    let Some(mut top) = player_by_id(server, target_id) else {
        return false;
    };
    let name = player.get_world().get_id();
    if top.get_world().get_id() != name
        || too_far(
            rider.get_position(),
            top.as_entity().get_position(),
            options.max_distance,
        )
    {
        return false;
    }
    let mut members = HashSet::new();
    let mut players = 0;
    let mut cursor = Some(top.as_entity());
    while let Some(e) = cursor {
        if e.get_id() == rider.get_id() || !members.insert(e.get_id()) {
            return false;
        }
        players += usize::from(matches!(e.get_type(), EntityType::Player));
        cursor = e.get_vehicle();
    }
    loop {
        let Some(next) = top.as_entity().get_passengers().into_iter().next() else {
            break;
        };
        if !members.insert(next.get_id()) || next.get_id() == rider.get_id() {
            return false;
        }
        let Some(p) = server.get_player_by_uuid(next.get_uuid()) else {
            return false;
        };
        players += 1;
        top = p;
    }
    let mut descendants = vec![player.as_entity()];
    while let Some(e) = descendants.pop() {
        if !members.insert(e.get_id()) {
            return false;
        }
        players += usize::from(matches!(e.get_type(), EntityType::Player));
        descendants.extend(e.get_passengers());
    }
    if (options.max_stack > 0 && players > options.max_stack)
        || !enabled(player, Preference::PlayerSit)
        || !enabled(&top, Preference::PlayerSit)
    {
        return false;
    }
    let Ok(_guard) = reserve(rider.get_id(), &name, None) else {
        return false;
    };
    forget_return(rider.get_id());
    let vehicle = top.as_entity();
    let vehicle_id = vehicle.get_id();
    let Ok(_top_guard) = reserve(vehicle_id, &name, None) else {
        return false;
    };
    if !vehicle.is_valid() || rider.get_vehicle().is_some() || !vehicle.get_passengers().is_empty()
    {
        return false;
    }
    let original = rider.get_position();
    vehicle.add_passenger(player.as_entity()).await;
    if !mounted(player, vehicle_id) {
        return false;
    }
    let surface = vehicle.get_position();
    let seat = Seat {
        rider: rider.get_id(),
        rider_uuid: player.get_id(),
        vehicle: vehicle_id,
        world: name,
        block: None,
        had_support: false,
        original,
        surface,
        vehicle_y: surface.1,
        activity: Activity::PlayerSit,
    };
    SEATS.with_borrow_mut(|m| m.insert(seat.rider, seat));
    true
}

/// Selects the saved starting position, the standing surface, or the base of a player stack.
fn return_position(seat: &Seat, world: &World, disconnect: bool) -> Option<Position> {
    let cfg = config();
    if seat.block.is_some() {
        let mut position = if cfg.get_up_return {
            seat.original
        } else {
            seat.surface
        };
        if disconnect {
            // Paper dismounts the departing passenger no lower than the removed vehicle.
            position.1 = position.1.max(seat.vehicle_y);
        }
        return Some(position);
    }
    if !cfg.player_sit.bottom_return {
        return None;
    }
    let mut bottom = world.get_entity_by_id(seat.vehicle as i32)?;
    let mut visited = HashSet::new();
    while visited.insert(bottom.get_id())
        && let Some(parent) = bottom.get_vehicle()
    {
        bottom = parent;
    }
    Some(bottom.get_position())
}

/// Removes only the tagged display owned by this session and closes its pose for all viewers.
async fn remove_visuals(seat: &Seat, player: &Player, server: &Server) {
    if seat.activity.is_pose() {
        pose::stop(player, server).await;
    }
    if seat.block.is_some()
        && let Some(world) = server.get_world_by_name(&seat.world)
        && let Some(entity) = world.get_entity_by_id(seat.vehicle as i32)
        && entity.has_scoreboard_tag(TAG)
    {
        entity.remove();
    }
}

/// Dismounts the tracked vehicle and waits for acknowledged movement before returning a live player.
pub async fn stop(player: &Player, server: &Server, reposition: bool) {
    let id = player.as_entity().get_id();
    let Some(seat) = SEATS.with_borrow_mut(|m| m.remove(&id)) else {
        return;
    };
    let world = player.get_world();
    if reposition
        && world.get_id() == seat.world
        && let Some(location) = return_position(&seat, &world, false)
    {
        // Keep this available to the leave callback even if the player quits during dismount.
        RETURNS.with_borrow_mut(|m| m.insert(id, (seat.world.clone(), location, false)));
    }
    if mounted(player, seat.vehicle) {
        player.as_entity().set_vehicle(None).await;
        if mounted(player, seat.vehicle) {
            forget_return(id);
            SEATS.with_borrow_mut(|m| m.insert(id, seat));
            return;
        }
    }
    remove_visuals(&seat, player, server).await;
}

/// Restores the standing position during the leave callback, before Pumpkin saves the player's NBT.
pub async fn disconnect(player: &Player, server: &Server) {
    let id = player.as_entity().get_id();
    let seat = SEATS.with_borrow_mut(|m| m.remove(&id));
    let pending = RETURNS.with_borrow_mut(|m| m.remove(&id));
    let world = player.get_world();
    let name = world.get_id();
    let position = match &seat {
        Some(seat) if seat.world == name => return_position(seat, &world, true),
        _ => pending
            .filter(|(w, _, _)| w == &name)
            .map(|(_, pos, _)| pos),
    };
    // Pumpkin already unlinked the passenger. A departing client cannot acknowledge a dismount.
    if let Some(position) = position {
        player.teleport(position, None, None, world).await;
    }
    if let Some(seat) = seat {
        remove_visuals(&seat, player, server).await;
    }
    eject(player, server).await;
}

/// Marks a pending return safe after Pumpkin accepts a real position change from this player.
pub fn accepted_move(player: &Player) {
    if RETURNS.with_borrow(|m| m.is_empty()) {
        return;
    }
    let id = player.as_entity().get_id();
    RETURNS.with_borrow_mut(|m| {
        if let Some(entry) = m.get_mut(&id) {
            entry.2 = true;
        }
    });
}

/// Abandons a queued return on world changes, a new mount, teleport, or disconnect.
pub fn forget_return(id: u32) {
    RETURNS.with_borrow_mut(|m| m.remove(&id));
}

/// Ejects only player passengers recorded as PumpGsit rides on this player.
pub async fn eject(player: &Player, server: &Server) {
    let id = player.as_entity().get_id();
    let riders = SEATS.with_borrow(|m| {
        m.values()
            .filter(|s| s.vehicle == id && s.activity == Activity::PlayerSit)
            .map(|s| s.rider)
            .collect::<Vec<_>>()
    });
    for rider in riders {
        if let Some(p) = player_by_id(server, rider) {
            stop(&p, server, false).await
        }
    }
}

/// Reconciles completed vanilla dismounts and supporting blocks changed by players or the world.
pub async fn reconcile(server: &Server) {
    let ready = RETURNS.with_borrow(|m| {
        m.iter()
            .filter(|(_, entry)| entry.2)
            .map(|(&id, _)| id)
            .collect::<Vec<_>>()
    });
    for id in ready {
        // World removal precedes the leave callback. Preserve its return until that callback runs.
        if let Some(player) = player_by_id(server, id)
            && let Some((world, location, _)) = RETURNS.with_borrow_mut(|m| m.remove(&id))
            && player.as_entity().get_vehicle().is_none()
            && player.get_world().get_id() == world
        {
            player
                .teleport(location, None, None, player.get_world())
                .await;
        }
    }
    let seats = SEATS.with_borrow(|m| m.values().cloned().collect::<Vec<_>>());
    for seat in seats {
        if let Some(p) = server.get_player_by_uuid(seat.rider_uuid) {
            let lost_support = config().get_up_break
                && seat.had_support
                && seat.block.is_some_and(|(x, y, z)| {
                    p.get_world().get_block_state(BlockPos { x, y, z }).is_air
                });
            if !mounted(&p, seat.vehicle) || lost_support {
                stop(&p, server, true).await
            }
        }
    }
}

/// Prevents vanilla dismount only when the operator disabled getting up by sneaking.
pub fn veto_dismount(id: u32, vehicle: u32) -> bool {
    !config().get_up_sneak
        && SEATS.with_borrow(|m| {
            m.get(&id)
                .is_some_and(|s| s.vehicle == vehicle && s.block.is_some())
        })
}

/// Removes an abandoned private entity while preserving active sessions and unrelated displays.
pub fn cleanup_entities(entities: Vec<world::Entity>) {
    for entity in entities {
        if pose::is_orphan(&entity)
            || (matches!(entity.get_type(), EntityType::BlockDisplay)
                && (entity.has_scoreboard_tag(TAG) || entity.has_scoreboard_tag(LEGACY_TAG))
                && entity.get_passengers().is_empty())
        {
            entity.remove()
        }
    }
}

/// Detaches a PumpGsit rider before an external teleport issues its own position confirmation.
/// Pumpkin's built-in ride command unlinks without sending a second player teleport.
/// Only a known plugin vehicle and a host-supplied Java profile name may use this path.
pub async fn before_teleport(player: &Player, server: &Server) -> bool {
    let id = player.as_entity().get_id();
    let vehicle = SEATS.with_borrow(|s| s.get(&id).map(|s| s.vehicle));
    if let Some(vehicle) = vehicle
        && mounted(player, vehicle)
    {
        let name = player.get_name();
        if name.is_empty() || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
            return false;
        }
        server
            .execute_command(
                format!("ride {name} dismount"),
                crate::bindings::pumpkin::plugin::server::CommandSender::Console,
            )
            .await;
        if mounted(player, vehicle) {
            return false;
        }
    }
    true
}
