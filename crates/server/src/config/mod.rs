//! Operator configuration and immutable game-policy snapshot.
//! Deployment settings may be overridden by environment variables; game policy
//! comes only from the TOML file and compiled defaults. No runtime reload is supported.

pub mod deployment;
pub mod bounty;
pub mod game;
pub mod map;
pub mod scheduler;
pub mod lua;

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::path::Path;
use std::sync::OnceLock;
use game::GameConfig;

/// Strict root schema. Game fields are explicit because serde flatten cannot
/// reliably enforce deny_unknown_fields at both the root and nested levels.
#[derive(Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub server: deployment::ServerConfig,
    pub database: deployment::DatabaseConfig,
    pub development: deployment::DevelopmentConfig,
    pub logging: deployment::LoggingConfig,
    pub chat: deployment::ChatConfig,
    pub images: deployment::ImagesConfig,
    pub gateway: deployment::GatewayConfig,
    pub cdn: deployment::CdnConfig,
    pub scheduler: scheduler::SchedulerConfig,
    pub lua: lua::LuaConfig,
    pub bounty: bounty::BountyConfig,
    pub map: map::MapConfig,
    pub accounts: game::economy::AccountsConfig,
    pub characters: game::economy::CharactersConfig,
    pub market: game::economy::MarketConfig,
    pub trade: game::economy::TradeConfig,
    pub repair: game::economy::RepairConfig,
    pub store: game::economy::StoreConfig,
    pub missions: game::world::MissionsConfig,
    pub standings: game::world::StandingsConfig,
    pub combat: game::world::CombatConfig,
    pub npc: game::world::NpcConfig,
    pub crimewatch: game::world::CrimewatchConfig,
    pub mining: game::world::MiningConfig,
    pub salvage: game::SalvageConfig,
    pub exploration: game::world::ExplorationConfig,
    pub industry: game::economy::IndustryConfig,
    pub corporations: game::social::CorporationsConfig,
    pub contracts: game::social::ContractsConfig,
    pub clones: game::social::ClonesConfig,
    pub mail: game::social::MailConfig,
    pub calendar: game::social::CalendarConfig,
    pub space: game::social::SpaceConfig,
    pub gm: game::gm::GmConfig,
    #[serde(skip)]
    source: String,
}

// The file location is metadata, not part of the resolved configuration.
impl PartialEq for Config {
    fn eq(&self, other: &Self) -> bool {
        self.server == other.server
            && self.database == other.database
            && self.development == other.development
            && self.logging == other.logging
            && self.chat == other.chat
            && self.images == other.images
            && self.gateway == other.gateway
            && self.cdn == other.cdn
            && self.scheduler == other.scheduler
            && self.lua == other.lua
            && self.bounty == other.bounty
            && self.map == other.map
            && self.accounts == other.accounts
            && self.characters == other.characters
            && self.market == other.market
            && self.trade == other.trade
            && self.store == other.store
            && self.repair == other.repair
            && self.missions == other.missions
            && self.standings == other.standings
            && self.combat == other.combat
            && self.npc == other.npc
            && self.crimewatch == other.crimewatch
            && self.mining == other.mining
            && self.salvage == other.salvage
            && self.exploration == other.exploration
            && self.industry == other.industry
            && self.corporations == other.corporations
            && self.contracts == other.contracts
            && self.clones == other.clones
            && self.mail == other.mail
            && self.calendar == other.calendar
            && self.space == other.space
            && self.gm == other.gm
    }
}

impl Config {
    pub fn load() -> Result<Self> {
        let path = std::env::var_os("CONFIG").unwrap_or_else(|| "./config.toml".into());
        Self::load_with(Path::new(&path), |key| {
            std::env::var_os(key).map(|value| value.to_string_lossy().into_owned())
        })
    }

    /// Resolve one file and deployment-only environment overrides without mutating
    /// process environment, so callers can exercise the same loader deterministically.
    pub(super) fn load_with(path: &Path, env: impl Fn(&str) -> Option<String>) -> Result<Self> {
        let (mut config, source) = match std::fs::read_to_string(path) {
            Ok(contents) => {
                let config = serde_path_to_error::deserialize(toml::Deserializer::new(&contents))
                    .map_err(|error| {
                        let parsed = contents.parse::<toml::Value>();
                        let message = error.inner().message();
                        let reason = if parsed.is_err() {
                            "invalid TOML syntax"
                        } else if message.starts_with("unknown field ") {
                            message
                        } else if message.starts_with("invalid type") {
                            "invalid type"
                        } else if message.starts_with("invalid length") {
                            "invalid length"
                        } else {
                            "invalid value"
                        };
                        let value = parsed.ok().and_then(|root| {
                            let mut current = &root;
                            for segment in error.path() {
                                current = match segment {
                                    serde_path_to_error::Segment::Map { key } => current.get(key.as_str())?,
                                    serde_path_to_error::Segment::Seq { index } => current.get(*index)?,
                                    _ => return None,
                                };
                            }
                            match current {
                                toml::Value::Integer(value) => Some(value.to_string()),
                                toml::Value::Float(value) => Some(value.to_string()),
                                toml::Value::Boolean(value) => Some(value.to_string()),
                                _ => None,
                            }
                        });
                        match value {
                            Some(value) => anyhow::anyhow!("invalid configuration {} at {}: {reason}: {value}", path.display(), error.path()),
                            None => anyhow::anyhow!("invalid configuration {} at {}: {reason}", path.display(), error.path()),
                        }
                    })?;
                (config, path.display().to_string())
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (Self::default(), "defaults".to_owned()),
            Err(error) => return Err(error).with_context(|| format!("reading configuration {}", path.display())),
        };
        config.source = source;
        macro_rules! override_env {
            ($key:literal, $field:expr) => {
                if let Some(value) = env($key) {
                    $field = value.parse().map_err(|_| anyhow::anyhow!("invalid {}: {:?}", $key, value))?;
                }
            };
        }
        macro_rules! secret_env {
            ($key:literal, $field:expr) => {
                if let Some(value) = env($key) { $field = value; }
            };
        }
        override_env!("BIND", config.server.bind);
        override_env!("PUBLIC_HOST", config.server.public_host);
        override_env!("NODE_ID", config.server.node_id);
        override_env!("SYSTEM_CAPACITY", config.server.system_capacity);
        override_env!("MACHO_VERSION", config.server.macho_version);
        override_env!("BOOT_VERSION", config.server.boot_version);
        override_env!("BOOT_BUILD", config.server.boot_build);
        override_env!("BOOT_CODENAME", config.server.boot_codename);
        override_env!("BOOT_REGION", config.server.boot_region);
        override_env!("RUST_LOG", config.logging.filter);
        override_env!("LOG_DIR", config.logging.dir);
        override_env!("DATABASE_HOST", config.database.host);
        override_env!("DATABASE_PORT", config.database.port);
        override_env!("DATABASE_USER", config.database.user);
        override_env!("DATABASE_NAME", config.database.name);
        override_env!("DATABASE_POOL_SIZE", config.database.pool_size);
        override_env!("CHAT_BIND", config.chat.bind);
        override_env!("CHAT_DOMAIN", config.chat.domain);
        override_env!("IMAGE_BIND", config.images.bind);
        override_env!("IMAGE_PORT", config.images.port);
        override_env!("IMAGE_DIR", config.images.dir);
        override_env!("IMAGE_UPSTREAM", config.images.upstream);
        override_env!("GATEWAY_BIND", config.gateway.bind);
        override_env!("CDN_BIND", config.cdn.bind);
        secret_env!("DATABASE_URL", config.database.url);
        secret_env!("DATABASE_PASSWORD", config.database.password);
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<()> {
        self.server.validate()?;
        self.database.validate()?;
        self.development.validate()?;
        self.logging.validate()?;
        self.chat.validate()?;
        self.images.validate()?;
        self.gateway.validate()?;
        self.cdn.validate()?;
        self.scheduler.validate()?;
        self.lua.validate()?;
        self.game_config().validate()?;
        Ok(())
    }

    pub fn source(&self) -> &str { &self.source }

    pub fn game_config(&self) -> GameConfig {
        GameConfig {
            bounty: self.bounty.clone(),
            map: self.map.clone(),
            accounts: self.accounts.clone(),
            characters: self.characters.clone(),
            market: self.market.clone(),
            trade: self.trade.clone(),
            store: self.store.clone(),
            repair: self.repair.clone(),
            missions: self.missions.clone(),
            standings: self.standings.clone(),
            combat: self.combat.clone(),
            npc: self.npc.clone(),
            crimewatch: self.crimewatch.clone(),
            mining: self.mining.clone(),
            salvage: self.salvage.clone(),
            exploration: self.exploration.clone(),
            industry: self.industry.clone(),
            corporations: self.corporations.clone(),
            contracts: self.contracts.clone(),
            clones: self.clones.clone(),
            mail: self.mail.clone(),
            calendar: self.calendar.clone(),
            space: self.space.clone(),
            gm: self.gm.clone(),
        }
    }

    pub fn cdn_url(&self) -> String { format!("http://{}", self.server.public_host) }

    /// machoNet global configuration handed to the client at handshake.
    pub fn global_config(&self) -> marshal::Value {
        marshal::Value::dict([
            ("imageserverurl", marshal::Value::str(format!("http://{}:{}/", self.server.public_host, self.images.port))),
            ("banned_words_enabled", marshal::Value::str(if self.chat.banned_words { "1" } else { "0" })),
        ])
    }
}

static GAME: OnceLock<GameConfig> = OnceLock::new();

pub fn init_game(config: GameConfig) -> Result<&'static GameConfig> {
    if GAME.set(config).is_err() { bail!("game configuration was already initialized"); }
    Ok(game())
}

pub fn game() -> &'static GameConfig {
    GAME.get().expect("game configuration must be initialized before use")
}

#[cfg(test)]
pub(crate) mod tests;
