//! Claims belonging to one immutable Lua generation. Old generations remain alive
//! until their in-flight calls finish; no client or database state is stored here.

use std::{collections::{HashMap, HashSet}, path::PathBuf, sync::Mutex};
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

#[derive(Default)]
pub struct Claims {
    pub rpc: HashMap<(String, String), Handler>,
    pub commands: HashMap<String, Command>,
    pub events: HashMap<String, Vec<Handler>>,
    pub intercepts: HashMap<(String, String), Handler>,
    pub scripts: Vec<String>,
    pub script_roots: HashMap<String, PathBuf>,
}

pub struct Registry {
    pub lua: Lua,
    pub overrides: Mutex<HashSet<(String, String)>>,
    pub claims: Claims,
}
