//! Brigadier command trees, Bukkit permission mappings, and player preference writes.
use crate::{
    bindings::{
        CommandError, CommandSender, ConsumedArgs, Context,
        pumpkin::plugin::{
            command::{Arg, ArgumentType, Command, CommandNode, StringType},
            permission::{Permission, PermissionChild, PermissionDefault, PermissionLevel},
            player::Player,
            server::Server,
            uuid,
        },
    },
    config, crawl, data_dir, hint, message,
    pose::{self, Pose},
    seat::{self, Activity},
    storage::{self, Preference},
};

const SIT: u32 = 1;
const CRAWL: u32 = 6;
const RELOAD: u32 = 7;
const HELP: u32 = 8;
const TOGGLE: u32 = 100;
const GROUPS: &[(&str, &[&str])] = &[
    ("sit.*", &["sit", "sitclick", "sittoggle"]),
    ("playersit.*", &["playersit", "playersittoggle"]),
    ("pose.*", &["lay", "legsup", "bellyflop", "spin"]),
    ("crawl.*", &["crawl", "crawlsneak", "crawltoggle"]),
    ("kick.*", &["kick.sit", "kick.pose"]),
    ("bypass.*", &["bypass.command", "bypass.world"]),
    (
        "*",
        &[
            "sit.*",
            "playersit.*",
            "pose.*",
            "crawl.*",
            "kick.*",
            "bypass.*",
            "reload",
        ],
    ),
];

/// Registers a permission, mapping Bukkit's GSit prefix into Pumpkin's required namespace.
fn permission(ctx: &Context, key: &str, operator: bool, children: &[&str]) -> Result<(), String> {
    let node = format!("PumpGsit:{key}");
    let registered = ctx.register_permission(&Permission {
        node: node.clone(),
        description: format!("PumpGsit {key}"),
        default: if operator {
            PermissionDefault::Op(PermissionLevel::Two)
        } else {
            PermissionDefault::Allow
        },
        children: children
            .iter()
            .map(|name| PermissionChild {
                node: format!("PumpGsit:{name}"),
                value: true,
            })
            .collect(),
    });
    // Pumpkin retains this registry across binary unload/load in the same server.
    match registered {
        Err(error) if error == format!("Permission {node} is already registered") => Ok(()),
        result => result,
    }
}

/// Builds toggle, explicit on/off, and console target branches without accepting ambiguous extra words.
fn toggle(command: &Command, literal: &str, base: u32) {
    let node = CommandNode::literal(literal);
    node.execute_with_handler_id(base);
    for (offset, name) in [(1, "on"), (2, "off")] {
        let value = CommandNode::literal(name);
        value.execute_with_handler_id(base + offset);
        node.then(value);
    }
    let target = CommandNode::argument("target", &ArgumentType::String(StringType::SingleWord));
    target.execute_with_handler_id(base + 3);
    for (offset, name) in [(4, "on"), (5, "off")] {
        let value = CommandNode::literal(name);
        value.execute_with_handler_id(base + offset);
        target.then(value);
    }
    node.then(target);
    command.then(node);
}

/// Registers seven command trees, including their original GSit aliases and permission groups.
pub fn register(ctx: &Context) -> Result<(), String> {
    for &(group, children) in GROUPS {
        if group != "*" {
            for leaf in children {
                permission(ctx, leaf, group == "kick.*" || group == "bypass.*", &[])?
            }
        }
        permission(ctx, group, true, children)?;
    }
    permission(ctx, "reload", true, &[])?;
    permission(ctx, "command", false, &[])?;
    let commands: &[(&[&str], &str, &str)] = &[
        (
            &["pumpgsit", "gsit", "sit"],
            "Sit down or set sitting preferences",
            "command",
        ),
        (&["glay", "lay", "glayback", "layback"], "Lie down", "lay"),
        (&["glegsup", "legsup"], "Rest with legs up", "legsup"),
        (&["gbellyflop", "bellyflop"], "Lie face down", "bellyflop"),
        (&["gspin", "spin"], "Spin in place", "spin"),
        (
            &["gcrawl", "crawl"],
            "Crawl or set crawling preferences",
            "command",
        ),
        (
            &["pumpgsitreload", "gsitreload", "gsitrl"],
            "Reload PumpGsit settings",
            "reload",
        ),
    ];
    for (i, (names, description, perm)) in commands.iter().enumerate() {
        let cmd = Command::new(
            &names.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
            description,
        );
        cmd.execute_with_handler_id(i as u32 + 1);
        if i == 0 {
            let help = CommandNode::literal("help");
            help.execute_with_handler_id(HELP);
            cmd.then(help);
            toggle(&cmd, "toggle", TOGGLE);
            toggle(&cmd, "playertoggle", TOGGLE + 10)
        }
        if i == 5 {
            toggle(&cmd, "toggle", TOGGLE + 20)
        }
        ctx.register_command(cmd, &format!("PumpGsit:{perm}"));
    }
    Ok(())
}

/// Returns the configured default used when the UUID is absent from the legacy table.
pub fn default_for(feature: Preference) -> bool {
    let cfg = config();
    match feature {
        Preference::Sit => cfg.sit.default_enabled,
        Preference::PlayerSit => cfg.player_sit.default_enabled,
        Preference::Crawl => cfg.crawl.default_enabled,
    }
}

/// Applies one preference operation and acknowledges it only after the database write succeeds.
async fn preferences(
    id: u32,
    sender: &CommandSender,
    server: &Server,
    args: &ConsumedArgs,
) -> Result<&'static str, &'static str> {
    let (pref, node, on, off) = match (id - TOGGLE) / 10 {
        0 => (Preference::Sit, "PumpGsit:sittoggle", "sit_on", "sit_off"),
        1 => (
            Preference::PlayerSit,
            "PumpGsit:playersittoggle",
            "stack_on",
            "stack_off",
        ),
        _ => (
            Preference::Crawl,
            "PumpGsit:crawltoggle",
            "crawl_on",
            "crawl_off",
        ),
    };
    if !sender.has_permission(node.into()).await {
        return Err("permission");
    }
    if (matches!(pref, Preference::PlayerSit) && !config().player_sit.allow_sit)
        || (matches!(pref, Preference::Crawl)
            && !config().crawl.corridor_crawl
            && !config().crawl.double_sneak)
    {
        return Err("disabled");
    }
    let mode = (id - TOGGLE) % 10;
    let target = match sender.as_player() {
        Some(player) if mode < 3 => player.get_id(),
        None if mode >= 3 => {
            let Arg::Simple(text) = args.get_value("target") else {
                return Err("target");
            };
            uuid::parse(&text)
                .or_else(|| server.get_player_by_name(&text).map(|p| p.get_id()))
                .ok_or("target")?
        }
        _ => return Err("target"),
    };
    let dir = data_dir();
    let key = uuid::to_string(target);
    let default = default_for(pref);
    let change = || -> Result<bool, String> {
        let value = match mode % 3 {
            0 => !storage::enabled(&dir, pref, &key, default)?,
            1 => true,
            _ => false,
        };
        storage::set_enabled(&dir, pref, &key, default, value)?;
        crate::forget_preferences(target);
        Ok(value)
    };
    match change() {
        Ok(true) => Ok(on),
        Ok(false) => Ok(off),
        Err(error) => {
            crate::warn(&error);
            Err("db")
        }
    }
}

/// Toggles a player posture after checking permission, current activity, world and server acceptance.
async fn posture(id: u32, player: &Player, server: &Server) -> Result<(), &'static str> {
    let (activity, perm, pose) = match id {
        SIT => (Activity::Sit, "PumpGsit:sit", None),
        2 => (Activity::Lay, "PumpGsit:lay", Some(Pose::Lay)),
        3 => (Activity::LegsUp, "PumpGsit:legsup", Some(Pose::LegsUp)),
        4 => (
            Activity::BellyFlop,
            "PumpGsit:bellyflop",
            Some(Pose::BellyFlop),
        ),
        5 => (Activity::Spin, "PumpGsit:spin", Some(Pose::Spin)),
        CRAWL => {
            if !player.has_permission("PumpGsit:crawl".into()).await {
                return Err("permission");
            }
            if crawl::active(player) {
                crawl::stop(player).await;
                return Ok(());
            }
            if !seat::allowed(player).await {
                return Err("world");
            }
            crawl::start(player)
                .await
                .map_err(|err| if err == "state" { "state" } else { "refused" })?;
            hint(player, config().crawl.get_up_sneak);
            return Ok(());
        }
        _ => return Err("state"),
    };
    if !player.has_permission(perm.into()).await {
        return Err("permission");
    }
    if seat::activity(player) == Some(activity) {
        seat::stop(player, server, true).await;
        return Ok(());
    }
    if pose.is_some() && player.as_java().is_none() {
        return Err("edition");
    }
    let surface = seat::start(player, server, activity, None).await?;
    if let Some(kind) = pose
        && let Err(error) = pose::start_at(player, kind, server, surface).await
    {
        crate::warn(&error);
        seat::stop(player, server, true).await;
        return Err("refused");
    }
    hint(player, config().get_up_sneak);
    Ok(())
}

/// Executes one registered handler and renders all responses in the sender's language.
pub async fn run(
    id: u32,
    sender: CommandSender,
    server: Server,
    args: ConsumedArgs,
) -> Result<i32, CommandError> {
    let player = sender.as_player();
    let reply = if id >= TOGGLE {
        preferences(id, &sender, &server, &args).await.map(Some)
    } else if id == HELP {
        Ok(Some("help"))
    } else if id == RELOAD {
        if !sender.has_permission("PumpGsit:reload".into()).await {
            Err("permission")
        } else {
            crate::reload(&server)
                .await
                .map(|()| Some("reload"))
                .map_err(|err| {
                    crate::warn(&err);
                    "bad_config"
                })
        }
    } else if let Some(ref p) = player {
        posture(id, p, &server).await.map(|()| None)
    } else {
        Err("player")
    };
    match reply {
        Ok(Some(key)) => {
            sender.send_message(message(player.as_ref(), key));
            Ok(1)
        }
        Ok(None) => Ok(1),
        Err(key) => {
            sender.send_error(message(player.as_ref(), key));
            Ok(0)
        }
    }
}
