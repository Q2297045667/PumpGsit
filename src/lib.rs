//! PumpGsit guest for Pumpkin's asynchronous component ABI.
#[allow(clippy::too_many_arguments)] // Function signatures are fixed by the host's WIT.
mod bindings {
    wit_bindgen::generate!({world:"plugin",path:"wit"});
}
mod command;
mod config;
mod crawl;
mod language;
mod pie;
mod pose;
mod seat;
mod state;
mod storage;

use bindings::pumpkin::plugin::{
    common::GameMode,
    event::{EntityInteractionAction, EventPriority, EventType, InteractAction},
    gametest::{self, BlockPermutation, Test},
    logging::{self, Level},
    player::Player,
    scheduler,
    server::Server,
    text::TextComponent,
    uuid,
};
use bindings::*;
use seat::Activity;
use state::State;
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, LazyLock},
};
use storage::Preference;

type Preferences = HashMap<(u64, u64, Preference), bool>;
static SETTINGS: State<Option<Arc<config::Config>>> = State::new(None);
static HOST: State<Option<Arc<Context>>> = State::new(None);
static TASK: State<Option<u32>> = State::new(None);
static PREFERENCES: LazyLock<State<Preferences>> = LazyLock::new(State::default);

/// Returns a settings snapshot without holding a guest borrow through a host callback.
fn config() -> Arc<config::Config> {
    SETTINGS.with_borrow(|cfg| cfg.as_ref().expect("PumpGsit loaded").clone())
}
/// Gets the loaded context without holding the state lock during a host call.
fn context() -> Arc<Context> {
    HOST.with_borrow(|ctx| ctx.as_ref().expect("PumpGsit loaded").clone())
}
/// Returns the plugin's WASI preopened data directory.
fn data_dir() -> PathBuf {
    PathBuf::from(context().get_data_folder())
}
/// Obtains an owned server handle for IPC callbacks that receive no server argument.
fn server() -> Server {
    context().get_server()
}
/// Reports operational errors to the server console.
fn warn(error: &str) {
    logging::log(Level::Warn, &format!("PumpGsit: {error}"))
}
/// Resolves an online player's preference, reading the database once per settings generation.
fn online_preference(player: &Player, feature: Preference) -> Result<bool, String> {
    let id = player.get_id();
    let key = (id.high, id.low, feature);
    if let Some(value) = PREFERENCES.with_borrow(|p| p.get(&key).copied()) {
        return Ok(value);
    }
    let value = storage::enabled(
        &data_dir(),
        feature,
        &uuid::to_string(id),
        command::default_for(feature),
    )?;
    PREFERENCES.with_borrow_mut(|p| p.insert(key, value));
    Ok(value)
}
/// Drops a player's three cached values after a write or disconnect.
fn forget_preferences(id: uuid::Uuid) {
    PREFERENCES.with_borrow_mut(|p| {
        for feature in [Preference::Sit, Preference::PlayerSit, Preference::Crawl] {
            p.remove(&(id.high, id.low, feature));
        }
    });
}
/// Matches GSit's enabled fallback on a read error without caching the failed query.
fn enabled(player: &Player, feature: Preference) -> bool {
    legacy_read(online_preference(player, feature))
}

/// GSit logs failed preference reads and allows the interaction until a later query succeeds.
fn legacy_read(result: Result<bool, String>) -> bool {
    result.unwrap_or_else(|error| {
        warn(&error);
        true
    })
}

/// Resolves a message using the recipient's locale and the configured fallback.
fn message(player: Option<&Player>, key: &str) -> TextComponent {
    let cfg = config();
    let client = player
        .filter(|_| cfg.client_language)
        .map(Player::get_locale);
    language::message(client, &cfg.language, key)
}

/// Sends the appropriate upstream action-bar message after the host accepts a posture.
fn hint(player: &Player, key: &str) {
    if config().custom_message {
        player.send_system_message(message(Some(player), key), true);
    }
}
/// Checks whether this player's active seat currently renders a PumpGsit pose.
fn posing(player: &Player) -> bool {
    seat::activity(player).is_some_and(Activity::is_pose)
}
/// Clears owned effects without relocating a dead or teleporting player.
async fn cleanup(player: &Player, server: &Server) {
    seat::forget_return(player.as_entity().get_id());
    crawl::cleanup(player).await;
    seat::eject(player, server).await;
    seat::stop(player, server, false).await;
}
/// Clears all transient rides and poses on reload or unload.
async fn cleanup_all(server: &Server) {
    for p in server.get_all_players() {
        cleanup(&p, server).await
    }
}
/// Loads and validates the new file before touching running sessions.
async fn reload(server: &Server) -> Result<(), String> {
    let config = config::read(&data_dir())?;
    let translations = language::read(&data_dir())?;
    storage::initialize(&data_dir())?;
    cleanup_all(server).await;
    PREFERENCES.with_borrow_mut(HashMap::clear);
    language::install(translations, &config.language);
    SETTINGS.set(Some(Arc::new(config)));
    Ok(())
}
/// Registers only event callbacks used by the gameplay implementation.
fn events(ctx: &Context) {
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
async fn dispatch(server: &Server, event: &mut Event) {
    match event {
        Event::PlayerInteractEvent(e) if !e.cancelled => {
            if posing(&e.player) && !config().pose.interact {
                e.cancelled = true;
                return;
            }
            if matches!(e.action, InteractAction::RightClickBlock)
                && let Some(pos) = e.clicked_pos
                && let Some(hit) = seat::click_target(&e.player, pos)
                && e.player.has_permission("PumpGsit:sitclick".into()).await
                && enabled(&e.player, Preference::Sit)
                && seat::start(&e.player, server, Activity::Sit, Some((pos, hit)))
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
        Event::PlayerToggleSneakEvent(e) if !e.cancelled => {
            // Accepted sneak input dismounts through Pumpkin immediately after this callback.
            // Reconcile the actual ride after that change, allowing other listeners to cancel.
            if seat::activity(&e.player).is_none() {
                if e.is_sneaking
                    && config().player_sit.sneak_ejects
                    && e.player.as_entity().get_vehicle().is_none()
                {
                    seat::eject(&e.player, server).await
                }
                let cfg = config();
                let can_start = e.is_sneaking
                    && !crawl::active(&e.player)
                    && (cfg.crawl.corridor_crawl || cfg.crawl.double_sneak)
                    && e.player.has_permission("PumpGsit:crawlsneak".into()).await
                    && seat::allowed(&e.player).await
                    && enabled(&e.player, Preference::Crawl);
                crawl::sneak(&e.player, e.is_sneaking, can_start).await;
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
            if crawl::active(&e.player) {
                crawl::moved(&e.player, e.to_position).await
            }
        }
        Event::PlayerLeaveEvent(e) => {
            forget_preferences(e.player.get_id());
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
            if matches!(
                e.damage_type,
                bindings::pumpkin::plugin::damage_types::DamageType::Fall
            ) && seat::activity_by_id(e.entity_id as u32) == Some(Activity::PlayerSit)
            {
                e.cancelled = true;
                return;
            }
            pose::hurt(e.entity_id, server).await;
            if !config().get_up_damage
                || e.damage <= 0.0
                || seat::activity_by_id(e.entity_id as u32) == Some(Activity::PlayerSit)
            {
                return;
            }
            if let Some(player) = seat::player_by_id(server, e.entity_id as u32) {
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

struct PumpGsit;
impl exports::pumpkin::plugin::metadata::Guest for PumpGsit {
    /// Supplies a pure metadata snapshot for the host's loader and permission prompt.
    fn get_metadata() -> exports::pumpkin::plugin::metadata::PluginMetadata {
        exports::pumpkin::plugin::metadata::PluginMetadata {
            name: env!("CARGO_PKG_NAME").into(),
            version: option_env!("PUMPGSIT_BUILD_VERSION")
                .unwrap_or(env!("CARGO_PKG_VERSION"))
                .into(),
            authors: vec!["PumpGsit contributors (based on GSit by Gecolay)".into()],
            description: env!("CARGO_PKG_DESCRIPTION").into(),
            dependencies: vec![],
            permissions: vec!["fs.read.data".into(), "fs.write.data".into()],
        }
    }
}
impl Guest for PumpGsit {
    /// Global installation is deferred until WASI resources are available.
    fn init_plugin() {}
    /// Opens settings and storage, then publishes commands, translations and listeners.
    async fn on_load(ctx: Context) -> Result<(), String> {
        let dir = PathBuf::from(ctx.get_data_folder());
        let cfg = config::read(&dir)?;
        storage::initialize(&dir)?;
        language::install(language::read(&dir)?, &cfg.language);
        SETTINGS.set(Some(Arc::new(cfg)));
        let ctx = Arc::new(ctx);
        HOST.set(Some(Arc::clone(&ctx)));
        command::register(&ctx)?;
        events(&ctx);
        for world in ctx.get_server().get_all_worlds() {
            seat::cleanup_entities(world.get_entities())
        }
        TASK.set(Some(scheduler::schedule_repeating_task(1, 1, 1)));
        Ok(())
    }
    /// Cancels the tick task and removes owned entities before the component disappears.
    async fn on_unload(ctx: Context) -> Result<(), String> {
        if let Some(task) = TASK.take() {
            scheduler::cancel_task(task)
        }
        cleanup_all(&ctx.get_server()).await;
        HOST.take();
        Ok(())
    }
    /// Reconciles server-initiated dismounts and refreshes viewers of active poses.
    async fn handle_task(_: u32, server: Server) {
        seat::reconcile(&server).await;
        pose::tick(&server).await;
        pie::refresh().await;
    }
    /// Returns the modified event so Pumpkin can honor blocking cancellations.
    async fn handle_event(_: u32, server: Server, mut event: Event) -> Event {
        dispatch(&server, &mut event).await;
        event
    }
    /// Routes Brigadier's selected command node to the corresponding PumpGsit operation.
    async fn handle_command(
        id: u32,
        sender: CommandSender,
        server: Server,
        args: ConsumedArgs,
    ) -> Result<i32, CommandError> {
        command::run(id, sender, server, args).await
    }
    /// Native Brigadier literals supply completions without invoking the guest on simulation threads.
    async fn handle_command_suggestion(
        _: u32,
        _: CommandSender,
        _: Server,
        request: SuggestionRequest,
    ) -> CommandSuggestions {
        CommandSuggestions {
            start: request.cursor,
            length: 0,
            values: vec![],
        }
    }
    /// Answers the optional PumpkinPIE expansion callback using current gameplay and SQLite state.
    async fn handle_ipc_message(sender: String, msg: Vec<u8>) -> Result<Vec<u8>, String> {
        pie::handle(sender, msg)
    }
    /// No custom AI goals are registered.
    async fn handle_ai_goal_can_start(_: u32, _: Server, _: Entity) -> bool {
        false
    }
    /// No custom AI goal can remain active.
    async fn handle_ai_goal_should_continue(_: u32, _: Server, _: Entity) -> bool {
        false
    }
    /// Receives the unused goal start callback required by the ABI.
    async fn handle_ai_goal_start(_: u32, _: Server, _: Entity) {}
    /// Receives the unused goal stop callback required by the ABI.
    async fn handle_ai_goal_stop(_: u32, _: Server, _: Entity) {}
    /// Receives the unused goal tick callback required by the ABI.
    async fn handle_ai_goal_tick(_: u32, _: Server, _: Entity) {}
    /// PumpGsit contributes no world generation phase.
    async fn handle_generate_phase(_: u32, _: GenerationPhase, _: ChunkBuffer) {}
}

impl exports::pumpkin::plugin::gametest_callbacks::Guest for PumpGsit {
    /// Receives the unused void GameTest callback.
    fn invoke_void(_: gametest::VoidCallbackId) {}
    /// Receives the unused synchronous GameTest callback.
    fn invoke_test(_: gametest::TestCallbackId, _: Test) {}
    /// Receives the unused asynchronous GameTest callback.
    async fn invoke_async_test(_: gametest::AsyncTestCallbackId, _: Test) {}
    /// No block predicates are registered.
    fn invoke_block_predicate(_: gametest::BlockPredicateCallbackId, _: BlockPermutation) -> bool {
        false
    }
    /// No entity predicates are registered.
    fn invoke_entity_predicate(_: gametest::EntityPredicateCallbackId, _: &Entity) -> bool {
        false
    }
}
bindings::export!(PumpGsit with_types_in bindings);
