//! Gameplay listeners registered with Pumpkin and the event cancellation rules they enforce.
use super::{
    cleanup, crawl, pose,
    seat::{self, Activity},
};
use crate::{
    bindings::{
        Context, Event,
        pumpkin::plugin::{
            common::GameMode,
            damage_types::DamageType,
            event::{EntityInteractionAction, EventPriority, EventType, InteractAction},
            player::Player,
            server::Server,
        },
    },
    config, hint, message, preferences,
};

/// Checks whether this player's active seat currently renders a PumpGsit pose.
fn posing(player: &Player) -> bool {
    seat::activity(player).is_some_and(Activity::is_pose)
}

/// Registers only event callbacks used by the gameplay implementation.
pub(crate) fn register(ctx: &Context) {
    use EventType::*;
    for ty in [
        PlayerInteractEvent,
        PlayerInteractEntityEvent,
        PlayerToggleSneakEvent,
        PlayerMoveEvent,
        EntityToggleSwimEvent,
        EntityDismountEvent,
        PlayerDeathEvent,
        PlayerLeaveEvent,
        PlayerChangeWorldEvent,
        PlayerTeleportEvent,
        PlayerGamemodeChangeEvent,
        PlayerToggleFlightEvent,
        PlayerCommandPreprocessEvent,
        PlayerCommandSendEvent,
        EntityDamageEvent,
        EntityDamageByEntityEvent,
        ProjectileLaunchEvent,
        InventoryClickEvent,
        InventoryCreativeEvent,
        PlayerDropItemEvent,
        PlayerAnimationEvent,
        EntitiesLoadEvent,
    ] {
        ctx.register_event(1, ty, EventPriority::Highest, true)
    }
}

/// Applies PumpGsit changes only to events that earlier handlers permitted.
pub(crate) async fn dispatch(server: &Server, event: &mut Event) {
    match event {
        Event::PlayerInteractEvent(e) if !e.cancelled => {
            if posing(&e.player) && !config().pose.interact {
                e.cancelled = true;
                return;
            }
            if matches!(e.action, InteractAction::RightClickBlock)
                && let Some(pos) = e.clicked_pos
                && seat::start(&e.player, server, Activity::Sit, Some(pos))
                    .await
                    .is_ok()
            {
                e.cancelled = true;
                hint(&e.player, "action-sit-info");
            }
        }
        Event::PlayerInteractEntityEvent(e) if !e.cancelled => {
            if posing(&e.player)
                && (!matches!(e.action, EntityInteractionAction::Attack) || !config().pose.interact)
            {
                e.cancelled = true
            } else if !e.sneaking
                && matches!(e.action, EntityInteractionAction::InteractAt)
                && seat::stack(&e.player, e.entity_id as u32, server).await
            {
                e.cancelled = true;
                hint(&e.player, "action-playersit-info");
            }
        }
        Event::PlayerToggleSneakEvent(e) if !e.cancelled && e.is_sneaking => {
            // Accepted sneak input dismounts through Pumpkin immediately after this callback.
            // Reconcile the actual ride after that change, allowing other listeners to cancel.
            if seat::activity(&e.player).is_none() {
                if config().player_sit.sneak_ejects
                    && !e.player.is_flying()
                    && e.player.as_entity().get_vehicle().is_none()
                {
                    seat::eject(&e.player, server).await
                }
                crawl::sneak(&e.player).await;
            }
        }
        Event::EntityToggleSwimEvent(e)
            if !e.is_swimming && crawl::prevent_swim_stop(e.entity_id) =>
        {
            e.cancelled = true
        }
        Event::EntityDismountEvent(e)
            if seat::veto_dismount(e.entity_id as u32, e.dismounted_id as u32) =>
        {
            e.cancelled = true
        }
        Event::PlayerMoveEvent(e) if !e.cancelled => {
            seat::accepted_move(&e.player);
            crawl::moved(&e.player, e.to_position).await
        }
        Event::PlayerLeaveEvent(e) => {
            preferences::forget(e.player.get_id());
            crawl::cleanup(&e.player).await;
            seat::disconnect(&e.player, server).await;
        }
        Event::PlayerDeathEvent(e) if !e.cancelled => cleanup(&e.player, server).await,
        Event::PlayerTeleportEvent(e) if !e.cancelled => {
            if seat::before_teleport(&e.player, server).await {
                cleanup(&e.player, server).await
            } else {
                e.cancelled = true
            }
        }
        Event::EntitiesLoadEvent(e) if !e.cancelled && e.entity_count > 0 => {
            for world in server.get_all_worlds() {
                if let Some(chunk) = world.get_chunk(e.chunk_x, e.chunk_z) {
                    seat::cleanup_entities(chunk.get_entities())
                }
            }
        }
        Event::PlayerChangeWorldEvent(e) if !e.cancelled => cleanup(&e.player, server).await,
        Event::PlayerGamemodeChangeEvent(e)
            if !e.cancelled && matches!(e.new_gamemode, GameMode::Spectator) =>
        {
            cleanup(&e.player, server).await
        }
        Event::PlayerToggleFlightEvent(e) if !e.cancelled && e.is_flying => {
            crawl::cleanup(&e.player).await
        }
        Event::PlayerAnimationEvent(e)
            if !e.cancelled && posing(&e.player) && e.animation_type.starts_with("ArmSwing") =>
        {
            pose::swing(&e.player, e.animation_type == "ArmSwingOff", server)
        }
        Event::EntityDamageEvent(e) if !e.cancelled => {
            let rider = seat::rider(e.entity_id as u32);
            if let Some((activity, uuid)) = rider {
                if activity == Activity::PlayerSit {
                    e.cancelled = matches!(e.damage_type, DamageType::Fall);
                    return;
                }
                if activity.is_pose() {
                    pose::hurt(uuid, server).await;
                }
            }
            if config().get_up_damage
                && e.damage > 0.0
                && let Some(uuid) = rider
                    .map(|(_, uuid)| uuid)
                    .or_else(|| crawl::owner(e.entity_id))
                && let Some(player) = server.get_player_by_uuid(uuid)
            {
                crawl::stop(&player).await;
                seat::stop(&player, server, true).await
            }
        }
        Event::EntityDamageByEntityEvent(e) if !e.cancelled && !config().pose.interact => {
            e.cancelled = seat::activity_by_id(e.damager_id as u32).is_some_and(Activity::is_pose)
        }
        Event::ProjectileLaunchEvent(e) if !e.cancelled && !config().pose.interact => {
            e.cancelled = e
                .shooter_id
                .and_then(|id| seat::activity_by_id(id as u32))
                .is_some_and(Activity::is_pose)
        }
        Event::InventoryClickEvent(e)
            if !e.cancelled
                && posing(&e.player)
                && matches!(e.player.get_gamemode(), GameMode::Creative) =>
        {
            e.cancelled = true
        }
        Event::InventoryCreativeEvent(e) if !e.cancelled && posing(&e.player) => e.cancelled = true,
        Event::PlayerDropItemEvent(e)
            if !e.cancelled
                && posing(&e.player)
                && matches!(e.player.get_gamemode(), GameMode::Creative) =>
        {
            e.cancelled = true
        }
        Event::PlayerCommandPreprocessEvent(e) if !e.cancelled => {
            e.cancelled = blocked(&e.player, &e.command).await
        }
        Event::PlayerCommandSendEvent(e) if !e.cancelled => {
            e.cancelled = blocked(&e.player, &e.command).await
        }
        _ => {}
    }
}

/// Applies the command blacklist during active PumpGsit actions with the upstream bypass.
async fn blocked(player: &Player, command: &str) -> bool {
    let name = command
        .trim_start_matches('/')
        .split_whitespace()
        .next()
        .unwrap_or_default();
    let denied = config()
        .command_blacklist
        .iter()
        .any(|cmd| name.eq_ignore_ascii_case(cmd.trim_start_matches('/')));
    if denied
        && seat::uses_command_blacklist(player)
        && !player
            .has_permission("PumpGsit:bypass.command".into())
            .await
    {
        player.send_system_message(message(Some(player), "action-blocked-error"), false);
        true
    } else {
        false
    }
}
