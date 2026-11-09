//! Claims belonging to one immutable Lua generation. Old generations remain alive
//! until their in-flight calls finish; no client or database state is stored here.

use std::{collections::{HashMap, HashSet}, path::PathBuf, sync::{Mutex, atomic::AtomicU64}, time::Duration};
use mlua::{Function, Lua};

#[derive(Clone)]
pub struct Handler {
    pub script: String,
    pub function: Function,
}

#[derive(Clone)]
pub struct Command {
    pub handler: Handler,
    pub help: String,
    pub gm: bool,
}

pub struct Tick {
    pub handler: Handler,
    pub name: String,
    pub interval: Duration,
    pub skipped: AtomicU64,
}

#[derive(Default)]
pub struct Claims {
    pub rpc: HashMap<(String, String), Handler>,
    pub gateway: HashMap<String, Handler>,
    pub commands: HashMap<String, Command>,
    pub events: HashMap<String, Vec<Handler>>,
    pub intercepts: HashMap<(String, String), Handler>,
    pub ticks: Vec<Tick>,
    pub scripts: Vec<String>,
    pub script_roots: HashMap<String, PathBuf>,
}

pub struct Registry {
    pub lua: Lua,
    pub overrides: Mutex<HashSet<(String, String)>>,
    pub claims: Claims,
    pub(super) content: super::content::ContentDefinitions,
}
