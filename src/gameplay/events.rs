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
    config, hint, message, preferences, warn,
};

/// Checks whether this player's active seat currently renders a PumpGsit pose.
fn posing(player: &Player) -> bool {
    seat::activity(player).is_some_and(Activity::is_pose)
}

// The resource has no type query. Use each event type as its callback ID so registration
// and dispatch share one list. A handler with a setter returns whether to cancel the event.
macro_rules! listeners {
    ($server:ident; $($kind:ident => $get:ident $(, $set:ident($value:expr))? : |$data:ident| $body:block)+) => {
        /// Registers each listener with its event type as the callback ID.
        pub(crate) fn register(ctx: &Context) {
            $(ctx.register_event(EventType::$kind as u32, EventType::$kind, EventPriority::Highest, true);)+
        }

        /// Reads the event payload and writes back a cancellation when the handler requests one.
        pub(crate) async fn dispatch(id: u32, $server: &Server, event: &Event) {
            match id {
                $(id if id == EventType::$kind as u32 => {
                    let Ok($data) = event.$get() else {
                        warn(concat!("Could not read ", stringify!($kind), " event"));
                        return;
                    };
                    listeners!(@handle event, $data $(, $set($value))?, $body);
                })+
                _ => {}
            }
        }
    };
    (@handle $event:ident, $data:ident, $body:block) => {
        $body
    };
    (@handle $event:ident, $data:ident, $set:ident($value:expr), $body:block) => {
        if !$data.cancelled && $body {
            let mut $data = $data;
            $data.cancelled = true;
            if $event.$set($value).is_err() {
                warn(concat!("Could not cancel event via ", stringify!($set)));
            }
        }
    };
}

listeners! {
    server;
    PlayerInteract => get_player_interact, set_player_interact(e): |e| {
        if posing(&e.player) && !config().pose.interact {
            true
        } else if matches!(e.action, InteractAction::RightClickBlock)
            && let Some(pos) = e.clicked_pos
            && seat::start(&e.player, server, Activity::Sit, Some(pos)).await.is_ok()
        {
            hint(&e.player, "action-sit-info");
            true
        } else {
            false
        }
    }
    PlayerInteractEntity => get_player_interact_entity, set_player_interact_entity(e): |e| {
        if posing(&e.player)
            && (!matches!(e.action, EntityInteractionAction::Attack) || !config().pose.interact)
        {
            true
        } else if !e.sneaking
            && matches!(e.action, EntityInteractionAction::InteractAt)
            && seat::stack(&e.player, e.entity_id as u32, server).await
        {
            hint(&e.player, "action-playersit-info");
            true
        } else {
            false
        }
    }
    PlayerToggleSneak => get_player_toggle_sneak: |e| {
        // Reconcile the ride after Pumpkin processes accepted sneak input and dismounts.
        if !e.cancelled && e.is_sneaking && seat::activity(&e.player).is_none() {
            if config().player_sit.sneak_ejects
                && !e.player.is_flying()
                && e.player.as_entity().get_vehicle().is_none()
            {
                seat::eject(&e.player, server).await;
            }
            crawl::sneak(&e.player).await;
        }
    }
    EntityToggleSwim => get_entity_toggle_swim, set_entity_toggle_swim(e): |e| {
        !e.is_swimming && crawl::prevent_swim_stop(e.entity_id)
    }
    EntityDismount => get_entity_dismount, set_entity_dismount(e): |e| {
        seat::veto_dismount(e.entity_id as u32, e.dismounted_id as u32)
    }
    PlayerMove => get_player_move: |e| {
        if !e.cancelled {
            seat::accepted_move(&e.player);
            crawl::moved(&e.player, e.to_position).await;
        }
    }
    PlayerLeave => get_player_leave: |e| {
        preferences::forget(e.player.get_id());
        crawl::cleanup(&e.player).await;
        seat::disconnect(&e.player, server).await;
    }
    PlayerDeath => get_player_death: |e| {
        if !e.cancelled {
            cleanup(&e.player, server).await;
        }
    }
    PlayerTeleport => get_player_teleport, set_player_teleport(e): |e| {
        if seat::before_teleport(&e.player, server).await {
            cleanup(&e.player, server).await;
            false
        } else {
            true
        }
    }
    EntitiesLoad => get_entities_load: |e| {
        if !e.cancelled && e.entity_count > 0 {
            for world in server.get_all_worlds() {
                if let Some(chunk) = world.get_chunk(e.chunk_x, e.chunk_z) {
                    seat::cleanup_entities(chunk.get_entities());
                }
            }
        }
    }
    PlayerChangeWorld => get_player_change_world: |e| {
        if !e.cancelled {
            cleanup(&e.player, server).await;
        }
    }
    PlayerGamemodeChange => get_player_gamemode_change: |e| {
        if !e.cancelled && matches!(e.new_gamemode, GameMode::Spectator) {
            cleanup(&e.player, server).await;
        }
    }
    PlayerToggleFlight => get_player_toggle_flight: |e| {
        if !e.cancelled && e.is_flying {
            crawl::cleanup(&e.player).await;
        }
    }
    PlayerAnimation => get_player_animation: |e| {
        if !e.cancelled && posing(&e.player) && e.animation_type.starts_with("ArmSwing") {
            pose::swing(&e.player, e.animation_type == "ArmSwingOff", server);
        }
    }
    EntityDamage => get_entity_damage, set_entity_damage(e): |e| {
        let rider = seat::rider(e.entity_id as u32);
        if let Some((Activity::PlayerSit, _)) = rider {
            matches!(e.damage_type, DamageType::Fall)
        } else {
            if let Some((activity, uuid)) = rider
                && activity.is_pose()
            {
                pose::hurt(uuid, server).await;
            }
            if config().get_up_damage
                && e.damage > 0.0
                && let Some(uuid) = rider
                    .map(|(_, uuid)| uuid)
                    .or_else(|| crawl::owner(e.entity_id))
                && let Some(player) = server.get_player_by_uuid(uuid)
            {
                crawl::stop(&player).await;
                seat::stop(&player, server, true).await;
            }
            false
        }
    }
    EntityDamageByEntity => get_entity_damage_by_entity, set_entity_damage_by_entity(&e): |e| {
        !config().pose.interact && seat::activity_by_id(e.damager_id as u32).is_some_and(Activity::is_pose)
    }
    ProjectileLaunch => get_projectile_launch, set_projectile_launch(e): |e| {
        !config().pose.interact
            && e.shooter_id
                .and_then(|id| seat::activity_by_id(id as u32))
                .is_some_and(Activity::is_pose)
    }
    InventoryClick => get_inventory_click, set_inventory_click(e): |e| {
        posing(&e.player) && matches!(e.player.get_gamemode(), GameMode::Creative)
    }
    InventoryCreative => get_inventory_creative, set_inventory_creative(e): |e| {
        posing(&e.player)
    }
    PlayerDropItem => get_player_drop_item, set_player_drop_item(e): |e| {
        posing(&e.player) && matches!(e.player.get_gamemode(), GameMode::Creative)
    }
    PlayerCommandPreprocess => get_player_command_preprocess, set_player_command_preprocess(e): |e| {
        blocked(&e.player, &e.command).await
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
