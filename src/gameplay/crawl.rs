//! The ground crawl uses Pumpkin's swimming flag for the server hitbox and
//! private, invisible shulkers to make the Java client predict the same pose.
use crate::bindings::pumpkin::plugin::{
    common::{BlockPos, GameMode, Position},
    java_packets::{CRemoveEntities, CSetEntityMetadata, CSpawnEntity, ClientboundPacket},
    player::{JavaMinecraftVersion, JavaPlayer, Player},
    uuid::Uuid,
    world::World,
};
use crate::state::State;
use std::{collections::HashMap, sync::LazyLock, time::Instant};

// Session-local client entity IDs are separate from Pumpkin's positive world entity IDs.
static CRAWLS: LazyLock<State<HashMap<i32, Crawl>>> = LazyLock::new(State::default);
static LAST_SNEAK: LazyLock<State<HashMap<i32, Instant>>> = LazyLock::new(State::default);
static NEXT_CLIENT_ID: State<i32> = State::new(-1_000_000_000);

#[derive(Clone, Copy, PartialEq)]
struct Ceiling {
    center: Position,
    peek: u8,
}

struct Crawl {
    first_id: i32,
    radius: i32,
    entered_corridor: Option<bool>,
    displayed: Option<Ceiling>,
    requested: Option<Ceiling>,
    updating: bool,
}

/// Returns the temporary server entity ID used by Pumpkin's swimming event.
fn id(player: &Player) -> i32 {
    player.as_entity().get_id() as i32
}

/// Reports whether this player has a pending or established PumpGsit crawl.
pub fn active(player: &Player) -> bool {
    prevent_swim_stop(id(player))
}

/// Keeps vanilla's out-of-water cleanup from cancelling the plugin's active crawl.
pub fn prevent_swim_stop(entity_id: i32) -> bool {
    CRAWLS.with_borrow(|crawls| crawls.contains_key(&entity_id))
}

/// Requires the same standing, unridden player as the original command and disallows
/// the environmental conditions under which PumpGsit immediately stops the crawl.
fn can_start(player: &Player) -> bool {
    let entity = player.as_entity();
    entity.is_valid()
        && entity.is_on_ground()
        && entity.get_vehicle().is_none()
        && !player.is_sleeping()
        && !player.is_flying()
        && !entity.is_in_water()
        && !matches!(player.get_gamemode(), GameMode::Spectator)
}

/// Starts a command-initiated crawl; the caller has already checked world and permission.
pub async fn start(player: &Player) -> Result<(), String> {
    begin(player, false).await
}

/// Reserves client IDs and enables actual swimming, rolling back if another listener vetoes it.
async fn begin(player: &Player, corridor: bool) -> Result<(), String> {
    if active(player) || !can_start(player) {
        return Err("state".into());
    }
    let radius = i32::from(crate::config().crawl.layers.max(1)) - 1;
    let count = (2 * radius + 1).pow(2);
    let first_id = NEXT_CLIENT_ID
        .with_borrow_mut(|next| {
            let first = *next;
            *next = next.checked_sub(count)?;
            Some(first)
        })
        .ok_or("refused")?;
    let key = id(player);
    CRAWLS.with_borrow_mut(|crawls| {
        crawls.insert(
            key,
            Crawl {
                first_id,
                radius,
                entered_corridor: corridor.then_some(false),
                displayed: None,
                requested: None,
                updating: false,
            },
        );
    });
    let entity = player.as_entity();
    entity.set_swimming(true).await;
    let ours =
        CRAWLS.with_borrow(|crawls| crawls.get(&key).is_some_and(|c| c.first_id == first_id));
    if !ours {
        if !active(player) && entity.is_swimming() {
            entity.set_swimming(false).await;
        }
        return Err("refused".into());
    }
    if !entity.is_swimming() {
        CRAWLS.with_borrow_mut(|crawls| crawls.remove(&key));
        return Err("refused".into());
    }
    refresh(player, player.get_position()).await;
    Ok(())
}

/// Removes the private collision entities and allows Pumpkin to select the normal pose
/// from the available headroom. An ordinary vanilla swim is left alone.
pub async fn stop(player: &Player) {
    let key = id(player);
    let Some(crawl) = CRAWLS.with_borrow_mut(|crawls| crawls.remove(&key)) else {
        return;
    };
    let entity = player.as_entity();
    entity.set_swimming(false).await;
    if (crawl.displayed.is_some() || crawl.updating)
        && let Some(java) = player.as_java()
    {
        remove(&java, crawl.first_id, crawl.radius).await;
    }
}

/// Forgets any double-sneak timer when the player leaves, changes world, or the plugin unloads.
pub async fn cleanup(player: &Player) {
    let key = id(player);
    LAST_SNEAK.with_borrow_mut(|sneaks| sneaks.remove(&key));
    stop(player).await;
}

/// Tests the exact half-block approach distance and cardinal orientation used by GSit.
fn corridor(world: &World, (x, y, z): Position, yaw: f32) -> bool {
    let (dx, dz) = match ((yaw.rem_euclid(360.0) + 45.0) / 90.0) as i32 % 4 {
        0 => (0, 1),
        1 => (-1, 0),
        2 => (0, -1),
        _ => (1, 0),
    };
    let front = BlockPos {
        x: x.floor() as i32 + dx,
        y: y.floor() as i32,
        z: z.floor() as i32 + dz,
    };
    (x + f64::from(dx) * 0.5).floor() as i32 == front.x
        && (z + f64::from(dz) * 0.5).floor() as i32 == front.z
        && world.get_block_state(front).collision_shapes.is_empty()
        && !world
            .get_block_state(BlockPos {
                y: front.y + 1,
                ..front
            })
            .collision_shapes
            .is_empty()
}

/// Handles rising sneak edges, prioritizing a configured downward double-sneak
/// ahead of corridor detection. Existing crawls use their independent get-up setting.
pub async fn sneak(player: &Player, entering: bool, enabled: bool) {
    if !entering {
        return;
    }
    let config = crate::config().crawl;
    if active(player) {
        if config.get_up_sneak {
            stop(player).await;
        }
        return;
    }
    if !enabled || !can_start(player) {
        return;
    }
    if config.double_sneak && player.get_pitch() >= config.double_sneak_pitch {
        let now = Instant::now();
        let key = id(player);
        let triggered = LAST_SNEAK.with_borrow_mut(|sneaks| {
            if sneaks.insert(key, now).is_some_and(|last| {
                now.duration_since(last).as_millis() <= u128::from(config.double_sneak_time_ms)
            }) {
                sneaks.remove(&key);
                true
            } else {
                false
            }
        });
        if triggered {
            let _ = begin(player, false).await;
        }
    } else if config.corridor_crawl
        && corridor(&player.get_world(), player.get_position(), player.get_yaw())
    {
        let _ = begin(player, true).await;
    }
}

/// Tracks passage through a one-block-high corridor and exits on the first move back
/// into headroom. Water and flight release all crawls, including command-initiated ones.
pub async fn moved(player: &Player, to: Position) {
    let key = id(player);
    let Some(entered) = CRAWLS.with_borrow(|crawls| crawls.get(&key).map(|c| c.entered_corridor))
    else {
        return;
    };
    if player.is_flying() || player.as_entity().is_in_water() {
        stop(player).await;
        return;
    }
    if let Some(entered) = entered {
        let world = player.get_world();
        let above = BlockPos {
            x: to.0.floor() as i32,
            y: to.1.floor() as i32 + 1,
            z: to.2.floor() as i32,
        };
        if world.get_block_state(above).collision_shapes.is_empty() {
            if entered || !corridor(&world, to, player.get_yaw()) {
                stop(player).await;
                return;
            }
        } else {
            CRAWLS.with_borrow_mut(|crawls| {
                if let Some(c) = crawls.get_mut(&key) {
                    c.entered_corridor = Some(true);
                }
            });
        }
    }
    refresh(player, to).await;
}

/// Computes the client collision surface at feet, slab and partial-block heights.
/// A real ceiling at the probe point makes the private collision surface unnecessary.
fn ceiling(player: &Player, (x, y, z): Position) -> Option<Ceiling> {
    let world = player.get_world();
    let feet = BlockPos {
        x: x.floor() as i32,
        y: y.floor() as i32,
        z: z.floor() as i32,
    };
    let fraction = ((y - y.floor()) * 100.0) as u8;
    let above = BlockPos {
        y: feet.y + if fraction >= 40 { 2 } else { 1 },
        ..feet
    };
    let probe = (x - x.floor(), 0.49, z - z.floor());
    if world
        .get_block_state(above)
        .collision_shapes
        .iter()
        .any(|shape| {
            shape.min.0 <= probe.0
                && probe.0 < shape.max.0
                && shape.min.1 <= probe.1
                && probe.1 < shape.max.1
                && shape.min.2 <= probe.2
                && probe.2 < shape.max.2
        })
    {
        return None;
    }
    let shaped_floor = world
        .get_block_state(feet)
        .collision_shapes
        .iter()
        .any(|shape| shape.max.1 - shape.min.1 >= 0.4);
    let raised = (shaped_floor || y % 0.015625 == 0.0)
        && player.as_entity().get_fall_distance() <= 0.7
        && fraction >= 40;
    Some(Ceiling {
        center: (x, y + if raised { 1.5 } else { 0.5 }, z),
        peek: if raised { 100 - fraction } else { 0 },
    })
}

/// Serializes overlapping movement callbacks for one client. A stopped or replaced
/// session removes its own IDs after any already-in-flight spawn packets finish.
async fn refresh(player: &Player, to: Position) {
    let Some(java) = player.as_java() else {
        return;
    };
    if java.get_version() != JavaMinecraftVersion::V263 {
        return;
    }
    let key = id(player);
    let target = ceiling(player, to);
    let work = CRAWLS.with_borrow_mut(|crawls| {
        let c = crawls.get_mut(&key)?;
        c.requested = target;
        if c.updating || c.displayed == target {
            return None;
        }
        c.updating = true;
        Some((c.first_id, c.radius, c.displayed))
    });
    let Some((first, radius, mut shown)) = work else {
        return;
    };
    loop {
        let next = CRAWLS.with_borrow_mut(|crawls| {
            let c = crawls.get_mut(&key).filter(|c| c.first_id == first)?;
            c.displayed = shown;
            if shown == c.requested {
                c.updating = false;
            }
            Some(c.requested)
        });
        let Some(next) = next else {
            if shown.is_some() {
                remove(&java, first, radius).await;
            }
            return;
        };
        if next == shown {
            return;
        }
        if let Some(next) = next {
            place(&java, player.get_id(), first, radius, shown, next).await;
        } else {
            remove(&java, first, radius).await;
        }
        shown = next;
    }
}

/// Updates or creates client-only shulkers. No helper entity is saved to the world
/// or broadcast to other players. Metadata indices/types are those of Java 26.3.
async fn place(
    java: &JavaPlayer,
    owner: Uuid,
    first: i32,
    radius: i32,
    old: Option<Ceiling>,
    next: Ceiling,
) {
    let mut id = first;
    for x in -radius..=radius {
        for z in -radius..=radius {
            let position = (
                next.center.0 + f64::from(x),
                next.center.1,
                next.center.2 + f64::from(z),
            );
            if old.is_none() {
                java.send_packet(ClientboundPacket::CSpawnEntity(CSpawnEntity {
                    entity_id: id,
                    entity_uuid: Uuid {
                        high: owner.high ^ 0x47534954,
                        low: owner.low ^ u64::from(id as u32),
                    },
                    r_type: 115,
                    position,
                    velocity: "[0,0,0]".into(),
                    pitch: 0,
                    yaw: 0,
                    head_yaw: 0,
                    data: 0,
                }))
                .await;
            }
            if old.is_none_or(|previous| previous.peek != next.peek) {
                java.send_packet(ClientboundPacket::CSetEntityMetadata(CSetEntityMetadata {
                    entity_id: id,
                    metadata: vec![
                        0, 0, 0x20, // Shared flags: invisible.
                        4, 8, 1, 5, 8, 1, // Silent, no gravity.
                        15, 0, 1, // Mob flags: no AI.
                        16, 12, 1, // Attach to the ceiling (Direction.UP).
                        17, 0, next.peek, 0xff,
                    ],
                }))
                .await;
            }
            if old.is_some() {
                move_box(java, id, position);
            }
            id -= 1;
        }
    }
}

/// Sends an absolute move via Pumpkin's Java player API. Its v0.2 typed serializer
/// omits CTeleportEntity; Java 26.3 packet 128 carries a VarInt, six doubles,
/// two floats, a zero relative-flags bitset and the on-ground boolean.
fn move_box(java: &JavaPlayer, id: i32, position: Position) {
    let mut bytes = Vec::with_capacity(66);
    let mut value = id as u32;
    while value >= 128 {
        bytes.push((value as u8 & 127) | 128);
        value >>= 7;
    }
    bytes.push(value as u8);
    for number in [position.0, position.1, position.2, 0.0, 0.0, 0.0] {
        bytes.extend_from_slice(&number.to_be_bytes());
    }
    bytes.extend_from_slice(&[0; 13]);
    java.send_raw_packet(128, &bytes);
}

/// Removes every ID in this session in one typed packet to the owning player.
async fn remove(java: &JavaPlayer, first: i32, radius: i32) {
    let count = (2 * radius + 1).pow(2);
    java.send_packet(ClientboundPacket::CRemoveEntities(CRemoveEntities {
        entity_ids: (0..count).map(|offset| first - offset).collect(),
    }))
    .await;
}
