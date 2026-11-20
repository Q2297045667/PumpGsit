//! Per-viewer Java 26.3 avatars for the four stationary poses. The real player
//! remains mounted on the server and keeps their camera, inventory and entity flags.
mod protocol;

use crate::bindings::pumpkin::plugin::{
    attributes::Attribute,
    common::{BlockPos, Hand, Position},
    entity_types::EntityType,
    item_stack::ItemStack,
    java_packets::{self as packets, ClientboundPacket as Packet},
    player::{JavaMinecraftVersion, JavaPlayer, Player},
    server::Server,
    sounds::{Sound, SoundCategory},
    statistics::CustomStatistic,
    uuid::Uuid,
    world::{self, Entity, EquipmentSlot, World},
};
use crate::state::State;
use std::{
    collections::{BTreeMap, BTreeSet, btree_map::Entry},
    sync::Arc,
};

const MARKER_TAG: &str = "pumpgsit.pose_marker";
const LEGACY_MARKER_TAG: &str = "gsit.pose_marker";

/// An original GSit stationary player pose.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Pose {
    Lay,
    LegsUp,
    BellyFlop,
    Spin,
}

impl Pose {
    /// Both sleeping animations get their orientation from a client-only bed.
    fn sleeping(self) -> bool {
        matches!(self, Self::Lay | Self::LegsUp)
    }
}

/// A server-only Marker reserves each virtual ID through Pumpkin's entity allocator.
struct Actor {
    handle: Entity,
    id: u32,
    uuid: Uuid,
}

impl Actor {
    /// Allocates an ID, validating that another plugin allowed the entity to spawn.
    async fn reserve(world: &World, at: Position) -> Result<Self, String> {
        let handle = world.spawn_entity(EntityType::Marker, at).await;
        let uuid = handle.get_uuid();
        if world.get_entity_by_uuid(uuid).is_none() {
            return Err("virtual entity allocation cancelled".into());
        }
        MARKERS.with_borrow_mut(|live| live.insert(key(uuid)));
        handle.add_scoreboard_tag(MARKER_TAG);
        Ok(Self {
            id: handle.get_id(),
            uuid,
            handle,
        })
    }
}

impl Drop for Actor {
    /// Removes the host reservation after all viewers have discarded the virtual entity.
    fn drop(&mut self) {
        self.handle.remove();
        MARKERS.with_borrow_mut(|live| live.remove(&key(self.uuid)));
    }
}

struct Session {
    pose: Pose,
    avatar: Actor,
    name_mask: Actor,
    footrest: Option<Actor>,
    profile: Vec<u8>,
    profile_id: Uuid,
    equipment: State<Option<Arc<Gear>>>,
    last_rotation: State<Option<(u8, u8, u8)>>,
    owner: Uuid,
    owner_entity: u32,
    world: u128,
    surface: Position,
    scale: f64,
    bed: BlockPos,
    bed_state: Option<u16>,
    quarter: u8,
    restore_sleep: bool,
    viewers: State<BTreeMap<u128, u32>>,
    ticks: State<u32>,
    closed: State<bool>,
}

static POSES: State<BTreeMap<u128, Arc<Session>>> = State::new(BTreeMap::new());
static MARKERS: State<BTreeSet<u128>> = State::new(BTreeSet::new());

/// Converts the host's two machine words to a comparable stable identifier.
fn key(id: Uuid) -> u128 {
    (u128::from(id.high) << 64) | u128::from(id.low)
}

/// Recreates the WIT UUID when looking a viewer up after they change world.
fn uuid(key: u128) -> Uuid {
    Uuid {
        high: (key >> 64) as u64,
        low: key as u64,
    }
}

/// Uses a fresh UUID with the same Java `UUID.hashCode` as the owner, so clients
/// with no signed texture choose the correct default skin and arm width.
fn profile_id(owner: Uuid, random: Uuid) -> Uuid {
    let hash =
        |id: Uuid| (id.high >> 32) as u32 ^ id.high as u32 ^ (id.low >> 32) as u32 ^ id.low as u32;
    Uuid {
        high: random.high,
        low: random.low ^ u64::from(hash(owner) ^ hash(random)),
    }
}

/// Captures appearance-bearing inventory slots once per owner, for all their viewers.
struct Gear(Vec<(EquipmentSlot, Option<ItemStack>)>);

impl Gear {
    /// Reads the player's selected item, off-hand and equipped armor without moving them.
    fn read(owner: &Player) -> Self {
        let inv = owner.get_inventory();
        Self(vec![
            (
                EquipmentSlot::MainHand,
                owner.get_inventory_item(owner.get_selected_slot()),
            ),
            (EquipmentSlot::OffHand, inv.get_off_hand()),
            (EquipmentSlot::Feet, inv.get_boots()),
            (EquipmentSlot::Legs, inv.get_leggings()),
            (EquipmentSlot::Chest, inv.get_chestplate()),
            (EquipmentSlot::Head, inv.get_helmet()),
        ])
    }

    /// Compares counts and full item component patches before resending equipment packets.
    async fn matches(&self, previous: &Gear) -> bool {
        for ((_, now), (_, before)) in self.0.iter().zip(&previous.0) {
            match (now, before) {
                (Some(a), Some(b))
                    if a.get_count().await == b.get_count().await
                        && a.is_similar(b.clone().await).await => {}
                (None, None) => (),
                _ => return false,
            }
        }
        true
    }

    /// Sends actual stacks to the proxy, or back to the real entity on cleanup.
    async fn show(&self, viewer: &Player, owner: u32, avatar: Option<u32>) {
        for (slot, stack) in &self.0 {
            let item = match stack {
                Some(stack) => Some(stack.clone().await),
                None => None,
            };
            if let Some(model) = avatar {
                viewer.send_equipment_change(model, *slot, item).await;
                viewer.send_equipment_change(owner, *slot, None).await;
            } else {
                viewer.send_equipment_change(owner, *slot, item).await;
            }
        }
    }

    /// Hides equipped items that Pumpkin's ordinary entity tracker can resend after a range change.
    async fn hide(&self, viewer: &Player, owner: u32) {
        for (slot, stack) in &self.0 {
            if stack.is_some() {
                viewer.send_equipment_change(owner, *slot, None).await;
            }
        }
    }
}

/// Encodes a full-turn angle using the same packed representation as the Java entity tracker.
fn angle(degrees: f32) -> u8 {
    (degrees.rem_euclid(360.) * 256. / 360.).floor() as u8
}

/// Returns only a connection whose entity metadata and packet layouts match this module.
fn client(player: &Player) -> Option<JavaPlayer> {
    player
        .as_java()
        .filter(|java| matches!(java.get_version(), JavaMinecraftVersion::V263))
}

/// Captures current entity flags so changing the render visibility preserves gameplay flags.
fn flags(entity: &Entity) -> u8 {
    u8::from(entity.is_on_fire())
        | (u8::from(entity.is_sneaking()) << 1)
        | (u8::from(entity.is_sprinting()) << 3)
        | (u8::from(entity.is_swimming()) << 4)
        | (u8::from(entity.is_invisible()) << 5)
        | (u8::from(entity.is_glowing()) << 6)
        | (u8::from(entity.is_fall_flying()) << 7)
}

/// Sends a metadata list including the terminating serializer sentinel.
async fn metadata(client: &JavaPlayer, id: u32, mut entries: Vec<u8>) {
    entries.push(0xff);
    client
        .send_packet(Packet::CSetEntityMetadata(packets::CSetEntityMetadata {
            entity_id: id as i32,
            metadata: entries,
        }))
        .await;
}

/// Spawns a packet-only player or zero-radius cloud on one client.
async fn spawn_actor(
    client: &JavaPlayer,
    actor: &Actor,
    entity_uuid: Uuid,
    kind: i32,
    pos: Position,
    yaw: f32,
    pitch: f32,
) {
    client
        .send_packet(Packet::CSpawnEntity(packets::CSpawnEntity {
            entity_id: actor.id as i32,
            entity_uuid,
            r_type: kind,
            position: pos,
            velocity: "[0,0,0]".into(),
            pitch: angle(pitch),
            yaw: angle(yaw),
            head_yaw: angle(yaw),
            data: 0,
        }))
        .await;
    if kind == 3 {
        metadata(client, actor.id, vec![8, 3, 0, 0, 0, 0, 5, 8, 1]).await;
    }
}

/// Links a virtual passenger on a single viewer's client.
async fn ride(client: &JavaPlayer, vehicle: u32, rider: u32) {
    client
        .send_packet(Packet::CSetPassengers(packets::CSetPassengers {
            entity_id: vehicle as i32,
            passengers: vec![rider as i32],
        }))
        .await;
}

impl Session {
    /// Takes a viewer snapshot before issuing callbacks that can re-enter the component.
    fn viewer_ids(&self) -> Vec<u128> {
        self.viewers
            .with_borrow(|viewers| viewers.keys().copied().collect())
    }

    /// The model offset is relative to the clicked surface, scaled like the original implementation.
    fn location(&self) -> Position {
        let offset = match self.pose {
            Pose::Lay | Pose::LegsUp => 0.1125,
            Pose::BellyFlop => -0.19,
            Pose::Spin => 0.0,
        };
        (
            self.surface.0,
            self.surface.1 + offset * self.scale,
            self.surface.2,
        )
    }

    /// Sends the player's skin layers, dominant arm and current pose to the model.
    async fn appearance(&self, player: &Player, java: &JavaPlayer, owner_flags: u8, first: bool) {
        let pose = match self.pose {
            Pose::Lay | Pose::LegsUp => 2,
            Pose::BellyFlop => 3,
            Pose::Spin => 4,
        };
        let hand = player.as_java().map(|java| java.get_settings().main_hand);
        let mut entries = vec![
            0,
            0,
            owner_flags & 0x61,
            6,
            20,
            pose,
            8,
            0,
            if self.pose == Pose::Spin { 4 } else { 0 },
            15,
            42,
            u8::from(!matches!(hand, Some(Hand::Left))),
            16,
            0,
            player.get_skin_parts().bits(),
        ];
        // The client snaps a sleeper to this block each time the accessor is
        // received. Only send it when spawning, before the position corrections.
        if first && self.pose.sleeping() {
            entries.extend_from_slice(&[14, 11, 1]);
            entries.extend_from_slice(&protocol::packed_position(self.bed));
        }
        metadata(java, self.avatar.id, entries).await;
    }

    /// Shows a newly tracked avatar, its pose-specific helpers and the owner's name mask.
    async fn introduce(&self, viewer: &Player, java: &JavaPlayer) {
        protocol::add_profile(java, &self.profile);
        spawn_actor(
            java,
            &self.avatar,
            self.profile_id,
            159,
            self.location(),
            0.,
            0.,
        )
        .await;
        if let Some(footrest) = &self.footrest {
            let yaw = [270., 180., 90., 0.][self.quarter as usize];
            spawn_actor(
                java,
                footrest,
                footrest.uuid,
                3,
                (
                    self.surface.0,
                    self.surface.1 - 0.375 * self.scale,
                    self.surface.2,
                ),
                yaw,
                0.,
            )
            .await;
            ride(java, footrest.id, self.avatar.id).await;
        }
        if let Some(state) = self.bed_state {
            viewer.send_block_change(self.bed, state);
        }
        if key(viewer.get_id()) == key(self.owner) {
            spawn_actor(
                java,
                &self.name_mask,
                self.name_mask.uuid,
                3,
                self.location(),
                0.,
                0.,
            )
            .await;
            ride(java, self.avatar.id, self.name_mask.id).await;
        }
        protocol::scale(java, self.avatar.id, self.scale);
    }

    /// Removes all pose-only objects and restores the authoritative block and player appearance.
    async fn dismiss(&self, viewer: &Player, owner: Option<&Player>, restore_bed: bool) {
        let Some(java) = client(viewer) else { return };
        java.send_packet(Packet::CRemovePlayerInfo(packets::CRemovePlayerInfo {
            players: vec![self.profile_id],
        }))
        .await;
        let mut ids = vec![self.avatar.id as i32];
        if let Some(footrest) = &self.footrest {
            ids.push(footrest.id as i32);
        }
        if key(viewer.get_id()) == key(self.owner) {
            ids.push(self.name_mask.id as i32);
        }
        java.send_packet(Packet::CRemoveEntities(packets::CRemoveEntities {
            entity_ids: ids,
        }))
        .await;
        if key(viewer.get_world().get_uuid()) != self.world {
            return;
        }
        if restore_bed && self.bed_state.is_some() {
            viewer.reset_block_change(self.bed);
        }
        if let Some(owner) = owner {
            metadata(
                &java,
                self.owner_entity,
                vec![0, 0, flags(&owner.as_entity())],
            )
            .await;
            Gear::read(owner)
                .show(viewer, self.owner_entity, None)
                .await;
        }
    }

    /// Tracks visibility before creating models and cleans up everyone who left the render range.
    async fn render(&self, owner: &Player, server: &Server) {
        let tick = self.ticks.with_borrow_mut(|next| {
            let tick = *next;
            *next = tick.wrapping_add(1);
            tick
        });
        let range = f64::from(server.get_simulation_distance()) * 16.;
        let owner_flags = flags(&owner.as_entity());
        let yaw = owner.get_yaw();
        let mut retained = BTreeSet::new();
        let previous = self.equipment.with_borrow(Clone::clone);
        let (gear, gear_changed) = match previous {
            Some(old) if !tick.is_multiple_of(5) => (old, false),
            old => {
                let new = Arc::new(Gear::read(owner));
                let changed = match old {
                    Some(old) => !new.matches(&old).await,
                    None => true,
                };
                self.equipment.set(Some(Arc::clone(&new)));
                (new, changed)
            }
        };
        let rotation = match self.pose {
            Pose::Spin => (0, 0, angle(-90.)),
            Pose::BellyFlop => (angle(yaw), angle(yaw), 0),
            _ => {
                let offset = [0., -90., -180., 90.][self.quarter as usize];
                let head = ((yaw + offset + 180.).rem_euclid(360.) - 180.).clamp(-45., 45.);
                (0, angle(head), 0)
            }
        };
        let turn_changed = self
            .last_rotation
            .with_borrow_mut(|last| last.replace(rotation))
            != Some(rotation);
        let sleep_config = crate::config().pose;
        let snore = self.pose.sleeping() && sleep_config.lay_snoring_sounds && {
            let day_tick = owner
                .get_player_time()
                .unwrap_or_else(|| owner.get_world().get_time_of_day())
                % 24000;
            day_tick.is_multiple_of(90)
                && (!sleep_config.lay_snoring_night_only || (12500..=23500).contains(&day_tick))
        };
        for viewer in server.get_players_in_world(owner.get_world()) {
            if self.closed.get() {
                return;
            }
            // Sending packets to earlier viewers can yield while another viewer switches worlds.
            if key(viewer.get_world().get_uuid()) != self.world {
                continue;
            }
            let Some(java) = client(&viewer) else {
                continue;
            };
            let id = key(viewer.get_id());
            let pos = viewer.get_position();
            let distance = (pos.0 - self.surface.0).powi(2)
                + (pos.1 - self.surface.1).powi(2)
                + (pos.2 - self.surface.2).powi(2);
            if distance > range * range {
                continue;
            }
            if id != key(self.owner)
                && !server
                    .get_player_by_uuid(self.owner)
                    .is_some_and(|other| viewer.can_see(other))
            {
                continue;
            }
            retained.insert(id);
            let since = self
                .viewers
                .with_borrow_mut(|viewers| *viewers.entry(id).or_insert(tick));
            let age = tick.wrapping_sub(since);
            if age == 0 {
                self.introduce(&viewer, &java).await;
            }
            if age == 0 || tick.is_multiple_of(5) {
                metadata(
                    &java,
                    self.owner_entity,
                    vec![0, 0, (owner_flags | 0x20) & !0x40],
                )
                .await;
                self.appearance(owner, &java, owner_flags, age == 0).await;
            }
            if age == 0 || gear_changed {
                gear.show(&viewer, self.owner_entity, Some(self.avatar.id))
                    .await;
            } else if age == 1 || tick.is_multiple_of(5) {
                gear.hide(&viewer, self.owner_entity).await;
            }
            if age == 0 || turn_changed {
                protocol::rotation(&java, self.avatar.id, rotation.0, rotation.1, rotation.2);
            }
            if self.pose.sleeping() && matches!(age, 2 | 4) {
                protocol::correct_sleeper(&java, self.avatar.id, self.location());
            }
            if snore {
                viewer.play_sound_at(
                    self.surface,
                    Sound::EntityFoxSleep,
                    SoundCategory::Players,
                    1.5,
                    0.,
                );
            }
        }
        let gone: Vec<_> = self.viewers.with_borrow_mut(|viewers| {
            viewers
                .extract_if(.., |id, _| !retained.contains(id))
                .map(|(id, _)| id)
                .collect()
        });
        for id in gone {
            if let Some(viewer) = server.get_player_by_uuid(uuid(id)) {
                self.dismiss(&viewer, Some(owner), true).await;
            }
        }
    }
}

/// Finds an independent fake bed in the seat's chunk. Vertically stacked sleepers
/// can then have different orientations and restore their blocks independently.
fn bed_location(world: u128, surface: Position, min_y: i32) -> BlockPos {
    let x = surface.0.floor() as i32;
    let z = surface.2.floor() as i32;
    let local = ((z & 15) << 4) | (x & 15);
    POSES.with_borrow(|poses| {
        let taken: BTreeSet<_> = poses
            .values()
            .filter(|s| s.world == world && s.pose.sleeping())
            .map(|s| (s.bed.x, s.bed.y, s.bed.z))
            .collect();
        for offset in 0..=taken.len() as i32 {
            let cell = (local + offset) % 256;
            let bed = BlockPos {
                x: (x & !15) + (cell % 16),
                y: min_y + offset / 256,
                z: (z & !15) + (cell / 16),
            };
            if !taken.contains(&(bed.x, bed.y, bed.z)) {
                return bed;
            }
        }
        unreachable!("one of N+1 distinct bed coordinates must be free")
    })
}

/// Starts a stationary pose at the supplied supporting surface, after the game's mount succeeds.
pub async fn start_at(
    player: &Player,
    pose: Pose,
    server: &Server,
    surface: Position,
) -> Result<(), String> {
    if client(player).is_none() {
        return Err("Java 26.3 required".into());
    }
    let world = player.get_world();
    let owner = player.get_id();
    let quarter = ((player.get_yaw() + 45.).rem_euclid(360.) / 90.).floor() as u8;
    let bed = bed_location(key(world.get_uuid()), surface, world.get_min_y());
    let bed_state = if pose.sleeping() {
        let facing = ["north", "east", "south", "west"][quarter as usize];
        let state = world::resolve_block_state(
            "minecraft:white_bed",
            &[
                ("facing".into(), facing.into()),
                ("part".into(), "head".into()),
                ("occupied".into(), "false".into()),
            ],
        );
        Some(state.ok_or_else(|| "white bed state is missing".to_string())?)
    } else {
        None
    };
    let living = player
        .as_entity()
        .as_living()
        .ok_or_else(|| "missing player entity".to_string())?;
    let avatar = Actor::reserve(&world, surface).await?;
    let name_mask = Actor::reserve(&world, surface).await?;
    let footrest = if pose == Pose::LegsUp {
        Some(Actor::reserve(&world, surface).await?)
    } else {
        None
    };
    let options = crate::config().pose;
    let profile_id = profile_id(owner, avatar.uuid);
    let session = Arc::new(Session {
        pose,
        profile: protocol::profile(player, profile_id),
        profile_id,
        equipment: State::new(None),
        last_rotation: State::new(None),
        avatar,
        name_mask,
        footrest,
        owner,
        owner_entity: player.as_entity().get_id(),
        world: key(world.get_uuid()),
        surface,
        scale: living.get_attribute_value(Attribute::Scale),
        bed,
        bed_state,
        quarter,
        restore_sleep: pose.sleeping() && options.lay_night_skip && !player.is_sleeping_ignored(),
        viewers: State::new(BTreeMap::new()),
        ticks: State::new(0),
        closed: State::new(false),
    });
    let started = POSES.with_borrow_mut(|poses| {
        if let Entry::Vacant(entry) = poses.entry(key(owner)) {
            entry.insert(Arc::clone(&session));
            true
        } else {
            false
        }
    });
    if !started {
        return Err("pose already active".into());
    }
    if session.restore_sleep {
        player.set_sleeping_ignored(true);
    }
    if pose.sleeping() && options.lay_rest {
        player.set_custom_statistic(CustomStatistic::TimeSinceRest, 0);
    }
    session.render(player, server).await;
    Ok(())
}

/// Ends the avatar session, restoring visible equipment, bed blocks and the owner's sleep setting.
pub async fn stop(player: &Player, server: &Server) {
    let owner = key(player.get_id());
    let session = POSES.with_borrow_mut(|poses| poses.remove(&owner));
    if let Some(session) = session {
        close(&session, Some(player), server).await;
    }
}

/// Clears client projections even if the owner has already disconnected.
async fn close(session: &Session, owner: Option<&Player>, server: &Server) {
    session.closed.set(true);
    if session.restore_sleep
        && let Some(player) = owner
    {
        player.set_sleeping_ignored(false);
    }
    let viewers = session.viewers.take();
    for id in viewers.into_keys() {
        if let Some(viewer) = server.get_player_by_uuid(uuid(id)) {
            session.dismiss(&viewer, owner, true).await;
        }
    }
}

/// Refreshes viewer membership and model state while the real owner is still available.
pub async fn tick(server: &Server) {
    let sessions: Vec<_> = POSES.with_borrow(|poses| poses.values().cloned().collect());
    for session in sessions {
        let owner = server.get_player_by_uuid(session.owner);
        if let Some(player) = &owner
            && key(player.get_world().get_uuid()) == session.world
            && player.as_entity().get_vehicle().is_some()
        {
            session.render(player, server).await;
        } else {
            let removed = POSES.with_borrow_mut(|poses| match poses.entry(key(session.owner)) {
                Entry::Occupied(entry) if Arc::ptr_eq(entry.get(), &session) => {
                    Some(entry.remove())
                }
                _ => None,
            });
            if let Some(removed) = removed {
                close(&removed, owner.as_ref(), server).await;
            }
        }
    }
}

/// Recognizes a marker left behind by an interrupted session, including one saved before the plugin rename.
pub fn is_orphan(entity: &Entity) -> bool {
    if !matches!(entity.get_type(), EntityType::Marker)
        || !(entity.has_scoreboard_tag(MARKER_TAG) || entity.has_scoreboard_tag(LEGACY_MARKER_TAG))
    {
        return false;
    }
    let marker = key(entity.get_uuid());
    !MARKERS.with_borrow(|live| live.contains(&marker))
}

/// Replays the real player's main- or off-hand success animation on all tracked avatars.
pub fn swing(player: &Player, off_hand: bool, server: &Server) {
    let owner = key(player.get_id());
    let session = POSES.with_borrow(|poses| poses.get(&owner).cloned());
    if let Some(session) = session {
        for id in session.viewer_ids() {
            if let Some(java) = server.get_player_by_uuid(uuid(id)).and_then(|p| client(&p)) {
                protocol::swing(&java, session.avatar.id, off_hand);
            }
        }
    }
}

/// Forwards a hit animation from an accepted damage event to each viewer's avatar.
pub async fn hurt(owner: Uuid, server: &Server) {
    let session = POSES.with_borrow(|poses| poses.get(&key(owner)).cloned());
    if let Some(session) = session {
        for id in session.viewer_ids() {
            if let Some(java) = server.get_player_by_uuid(uuid(id)).and_then(|p| client(&p)) {
                java.send_packet(Packet::CHurtAnimation(packets::CHurtAnimation {
                    entity_id: session.avatar.id as i32,
                    yaw: 0.,
                }))
                .await;
            }
        }
    }
}
