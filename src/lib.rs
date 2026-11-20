//! PumpGsit guest for Pumpkin's asynchronous component ABI.
#[allow(clippy::too_many_arguments)] // Function signatures are fixed by the host's WIT.
mod bindings {
    wit_bindgen::generate!({
        world: "plugin",
        path: "wit",
    });
}
mod command;
mod config;
mod gameplay;
mod integrations;
mod language;
mod preferences;
mod state;
mod storage;

use bindings::pumpkin::plugin::{
    gametest::{self, BlockPermutation, Test},
    logging::{self, Level},
    player::Player,
    scheduler,
    server::Server,
    text::TextComponent,
};
use bindings::{
    ChunkBuffer, CommandError, CommandSender, CommandSuggestions, ConsumedArgs, Context, Entity,
    Event, GenerationPhase, Guest, SuggestionRequest, exports,
};
use gameplay::{events, pose, seat};
use integrations::pumpkin_pie;
use state::State;
use std::{path::PathBuf, sync::Arc};

static SETTINGS: State<Option<Arc<config::Config>>> = State::new(None);
static HOST: State<Option<Arc<Context>>> = State::new(None);
static TASK: State<Option<u32>> = State::new(None);

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

/// Loads and validates the new file before touching running sessions.
async fn reload(server: &Server) -> Result<(), String> {
    let config = config::read(&data_dir())?;
    let translations = language::read(&data_dir())?;
    storage::initialize(&config.database, &data_dir())?;
    gameplay::cleanup_all(server).await;
    preferences::clear();
    language::install(translations, &config.language);
    SETTINGS.set(Some(Arc::new(config)));
    Ok(())
}
struct PumpGsit;
impl exports::pumpkin::plugin::metadata::Guest for PumpGsit {
    /// Supplies a pure metadata snapshot for the host's loader and permission prompt.
    fn get_metadata() -> exports::pumpkin::plugin::metadata::PluginMetadata {
        exports::pumpkin::plugin::metadata::PluginMetadata {
            name: env!("CARGO_PKG_NAME").into(),
            version: option_env!("PUMPKIN_PLUGIN_VERSION")
                .or(option_env!("PUMPGSIT_BUILD_VERSION"))
                .unwrap_or(env!("CARGO_PKG_VERSION"))
                .into(),
            authors: vec!["PumpGsit contributors (based on GSit by Gecolay)".into()],
            description: env!("CARGO_PKG_DESCRIPTION").into(),
            dependencies: vec![],
            permissions: vec![
                "fs.read.data".into(),
                "fs.write.data".into(),
                "network.tcp.connect".into(),
                "network.dns".into(),
            ],
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
        storage::initialize(&cfg.database, &dir)?;
        language::install(language::read(&dir)?, &cfg.language);
        SETTINGS.set(Some(Arc::new(cfg)));
        let ctx = Arc::new(ctx);
        HOST.set(Some(Arc::clone(&ctx)));
        command::register(&ctx)?;
        events::register(&ctx);
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
        gameplay::cleanup_all(&ctx.get_server()).await;
        HOST.take();
        Ok(())
    }
    /// Reconciles server-initiated dismounts and refreshes viewers of active poses.
    async fn handle_task(_: u32, server: Server) {
        seat::reconcile(&server).await;
        pose::tick(&server).await;
        pumpkin_pie::refresh().await;
    }
    /// Returns the modified event so Pumpkin can honor blocking cancellations.
    async fn handle_event(_: u32, server: Server, mut event: Event) -> Event {
        events::dispatch(&server, &mut event).await;
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
        pumpkin_pie::handle(sender, msg)
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
