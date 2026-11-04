//! GM slash-command and per-item dogma override messages.
//! The client slash service consumes these strings; they read no database and
//! implement neither gameplay behavior nor localisation.

/// An argument quote was not closed.
pub(crate) const GM_ARGS_UNCLOSED_QUOTE: &str = "引号未闭合。";
/// A command requires a selected character.
pub(crate) const GM_ARGS_NO_CHARACTER: &str = "当前没有选择角色。";
pub(crate) fn gm_args_missing(index: usize) -> String { format!("缺少第 {index} 个参数。") }
pub(crate) fn gm_args_integer(text: &str) -> String { format!("无效的整数：{text}") }
pub(crate) fn gm_args_number(text: &str) -> String { format!("无效的数值：{text}") }
pub(crate) fn gm_args_unknown_target(text: &str) -> String { format!("找不到角色：{text}") }
pub(crate) fn gm_args_unknown_owner(text: &str) -> String { format!("找不到所有者：{text}") }
pub(crate) fn gm_rows_truncated(count: usize) -> String { format!("另有 {count} 行未显示。") }

// GM slash commands (services/slash.rs)
/// Teleport command help.
pub(crate) const GM_HELP_TELEPORT: &str = "/tr me <itemID|characterID|solarSystemID|stationID|celestialID> — teleport";
/// Wormhole command help.
pub(crate) const GM_HELP_WORMHOLE: &str = "/wh [status|ensure|random|clear] — 虫洞操作；无参数时前往虫洞";
/// Scan-all command help.
pub(crate) const GM_HELP_SCAN_ALL: &str = "/scanall — scan every signature in this system to 100%";
/// Site listing command help.
pub(crate) const GM_HELP_SITES: &str = "/sites — 列出当前星系的异常和信号强度";
/// Create command help.
pub(crate) const GM_HELP_CREATE: &str = "/create <typeID|name> [qty] — into your hangar (docked) or cargo";
/// Create-item command help.
pub(crate) const GM_HELP_CREATE_ITEM: &str = "/createitem <typeID|name> [qty] — same as /create";
/// Set ISK command help.
pub(crate) const GM_HELP_SET_ISK: &str = "/setisk <amount> — set your wallet";
/// Add ISK command help.
pub(crate) const GM_HELP_ADD_ISK: &str = "/addisk <amount> — add ISK to your wallet";
/// Set PLEX command help.
pub(crate) const GM_HELP_SET_PLEX: &str = "/setplex <count> — set your PLEX vault";
/// Add PLEX command help.
pub(crate) const GM_HELP_ADD_PLEX: &str = "/addplex <count> — add PLEX to your vault";
/// Heal command help.
pub(crate) const GM_HELP_HEAL: &str = "/heal — repair your ship in space";
/// Help command help.
pub(crate) const GM_HELP_HELP: &str = "/help — this list";
/// Clone grade command help.
pub(crate) const GM_HELP_CLONE_GRADE: &str = "/clonegrade <user_id> alpha|omega [duration_seconds [grace_seconds]] — 设置账号克隆等级；省略时长则 Omega 无限期，宽限期默认为 0；时长必须为正数，宽限期不能为负数；Alpha 不接受时长参数。";
/// Clone grade change result.
pub(crate) fn clone_grade_changed(user: i64, grade: &str) -> String {
    format!("已将账号 {user} 的克隆等级设置为 {grade}。")
}
/// Teleport command usage error.
pub(crate) const GM_TELEPORT_USAGE: &str = "usage: /tr me <itemID|characterID|solarSystemID>";
/// Create command usage error.
pub(crate) const GM_CREATE_USAGE: &str = "usage: /create <typeID|name> [qty]";
/// Set ISK command usage error.
pub(crate) const GM_SET_ISK_USAGE: &str = "usage: /setisk <amount>";
/// Add ISK command usage error.
pub(crate) const GM_ADD_ISK_USAGE: &str = "usage: /addisk <amount>";
/// Set PLEX command usage error.
pub(crate) const GM_SET_PLEX_USAGE: &str = "usage: /setplex <count>";
/// Add PLEX command usage error.
pub(crate) const GM_ADD_PLEX_USAGE: &str = "usage: /addplex <count>";
/// Wallet numeric overflow.
pub(crate) const GM_AMOUNT_TOO_LARGE: &str = "amount exceeds the wallet limit";
/// No destination for a created item.
pub(crate) const GM_NOWHERE_TO_PUT_ITEM: &str = "nowhere to put it";
/// Docked ships repair automatically.
pub(crate) const GM_DOCKED_SHIP_REPAIRS: &str = "docked ships repair themselves";
/// Ship repair result.
pub(crate) const GM_REPAIRED: &str = "repaired";
/// Missing GM role.
pub(crate) const GM_ROLE_REQUIRED: &str = "you are not a GM";
/// Unknown item type.
pub(crate) fn gm_unknown_type(arg: &str) -> String { format!("no such type: {arg}") }
/// Invalid destination ID.
pub(crate) fn gm_invalid_id(dest: &str) -> String { format!("not an ID: {dest}") }
/// Teleport result.
pub(crate) fn gm_teleported(id: i64) -> String { format!("teleported to {id}") }
/// Unknown solar system.
pub(crate) fn gm_unknown_system(id: i64) -> String { format!("unknown solar system {id}") }
/// Solar-system teleport result.
pub(crate) fn gm_teleported_to_system(id: i64) -> String { format!("teleported to system {id}") }
/// Teleport to an item in another system.
pub(crate) fn gm_teleported_in_system(id: i64, system: i64) -> String { format!("teleported to {id} in {system}") }
/// Unknown teleport target.
pub(crate) fn gm_target_not_found(id: i64) -> String { format!("can't find {id}") }
/// Created item result.
pub(crate) fn gm_created_item(qty: i32, name: &str, type_id: i32) -> String { format!("created {qty} x {name} ({type_id})") }
/// Updated ISK balance, with a client projection when clamped.
pub(crate) fn gm_balance(exact: &str, shown: Option<f64>) -> String {
    let mut result = format!("balance {exact} ISK");
    if let Some(shown) = shown { result.push_str(&format!(" (client shows {shown:.0})")); }
    result
}
/// Updated PLEX vault, with a client projection when clamped.
pub(crate) fn gm_plex_balance(exact: &str, shown: Option<i64>) -> String {
    let mut result = format!("vault {exact} PLEX");
    if let Some(shown) = shown { result.push_str(&format!(" (client shows {shown})")); }
    result
}
/// One quoted command in the command list.
pub(crate) fn gm_command_name(n: &str) -> String { format!("'{n}'") }
/// Remote command list.
pub(crate) fn gm_command_list(names: &str) -> String { format!("commands: [{}]", names) }
/// Unknown command and help text.
pub(crate) fn gm_unknown_command(other: &str, help: &str) -> String { format!("unknown command /{other}\n{}", help) }

/// Effect diagnostics command help and usage.
pub(crate) const GM_HELP_EFFECTS: &str = "/effects unsupported — 列出未支持的效果";
/// Invalid effect diagnostics arguments.
pub(crate) const GM_EFFECTS_USAGE: &str = "用法：/effects unsupported";
/// No unsupported effects have been observed.
pub(crate) const GM_EFFECTS_EMPTY: &str = "尚未记录未支持的效果。";
/// Format the observed unsupported effects in snapshot order.
pub(crate) fn gm_effects_unsupported(effects: &[(String, i32)]) -> String {
    if effects.is_empty() {
        return GM_EFFECTS_EMPTY.into();
    }
    effects.iter().map(|(name, type_id)| format!("{name}（类型 ID：{type_id}）")).collect::<Vec<_>>().join("\n")
}

/// Native turret accuracy diagnostic.
pub(crate) const GM_HELP_HITCHANCE: &str = "/hitchance <目标ID> — 查看炮台命中率及计算输入";
pub(crate) const GM_HITCHANCE_USAGE: &str = "用法：/hitchance <目标ID>";
pub(crate) const GM_HITCHANCE_UNDOCK: &str = "请先驾驶飞船离开空间站。";
pub(crate) const GM_HITCHANCE_MISSING_BALL: &str = "当前飞船或目标不在当前星系中。";
pub(crate) const GM_HITCHANCE_EMPTY: &str = "当前飞船没有装配炮台。";
pub(crate) fn gm_hitchance_row(item: i64, type_id: i32, center: f64, surface: f64, speed: f64, angular: f64, angular_squared: f64, tracking: f64, opt_sig: f64, target_sig: f64, optimal: f64, falloff: f64, chance: f64) -> String {
    format!("炮台物品 {item}，类型 {type_id}：中心距离={center:.17e}，表面距离={surface:.17e}，相对速度={speed:.17e}，角速度={angular:.17e}，角速度平方={angular_squared:.17e}，追踪速度={tracking:.17e}，炮台信号半径={opt_sig:.17e}，目标信号半径={target_sig:.17e}，最佳射程={optimal:.17e}，失准范围={falloff:.17e}，命中率={chance:.17e}")
}

// GM per-item dogma overrides.
pub(crate) const GM_HELP_DOGMA: &str = "/dogma <ship|物品ID|flag:N> <属性ID|名称> [数值|reset]；/dogma <目标> list";
pub(crate) const GM_DOGMA_USAGE: &str = "用法：/dogma <ship|物品ID|flag:N> <属性ID|名称> [数值|reset]，或 /dogma <目标> list。";
pub(crate) const GM_DOGMA_NO_SHIP: &str = "没有当前飞船。";
pub(crate) const GM_DOGMA_SINGLETON: &str = "只能修改单件物品，请使用已组装的单件物品。";
pub(crate) const GM_DOGMA_FORBIDDEN_TARGET: &str = "不能修改弹药、角色、建筑或 NPC 的属性。";
pub(crate) const GM_DOGMA_RUNTIME: &str = "此属性由运行状态管理；护盾、装甲和结构请使用 /heal，在线状态请使用模块在线开关。";
pub(crate) const GM_DOGMA_CHARACTER: &str = "角色属性不能通过 /dogma 修改。";
pub(crate) const GM_DOGMA_ACTIVE_CYCLE: &str = "运行中的模块不能修改周期属性；请先关闭模块。";
pub(crate) const GM_DOGMA_VISIBILITY: &str = "模块或船体原本没有的属性不会显示在装配窗口和 HUD 最大值中。";
pub(crate) const GM_DOGMA_BASE_ONLY: &str = "目标不在当前装配中；仅显示基础值，实际有效值取决于其装配和驾驶员。";
pub(crate) const GM_DOGMA_EMPTY: &str = "没有属性覆盖。";
pub(crate) fn gm_dogma_invalid_target(s: &str) -> String { format!("无效的目标：{s}") }
pub(crate) fn gm_dogma_attribute(s: &str) -> String { format!("未知属性：{s}") }
pub(crate) fn gm_dogma_ambiguous(s: &str, ids: &str) -> String { format!("属性名称 {s} 不唯一，请使用 ID：{ids}") }
pub(crate) fn gm_dogma_result(item: i64, attr: &str, id: i32, old_base: f64, new_base: f64, effective: Option<(f64, f64)>, overridden: bool) -> String {
    let effective = effective.map(|(old, new)| format!("；有效值 {old} → {new}")).unwrap_or_default();
    format!("物品 {item} 属性 {attr} ({id})：基础值 {old_base} → {new_base}{effective}；覆盖：{}", if overridden { "是" } else { "否" })
}
pub(crate) fn gm_dogma_noop(item: i64, id: i32) -> String { format!("物品 {item} 的属性 {id} 没有覆盖，无需重置。") }
pub(crate) fn gm_dogma_list_item(flag: i32, name: &str, item: i64) -> String { format!("位置 {flag}，{name}，物品 {item}") }
pub(crate) fn gm_dogma_list_entry(id: i32, value: f64) -> String { format!("  属性 {id}: {value}") }

// Lua administration.
pub(crate) const LUA_HELP: &str = "用法：/lua reload 或 /lua list";
pub(crate) const LUA_RELOADED: &str = "脚本已重新加载。";
pub(crate) fn lua_reload_failed(error: &str) -> String { format!("脚本重新加载失败：{error}") }
pub(crate) fn lua_list(list: &str) -> String { format!("Lua 脚本：\n{list}") }

// Skill administration.
pub(crate) const GM_HELP_GIVESKILL: &str = "/giveskill <角色> <技能|all|super> [等级] — 设置技能等级";
pub(crate) const GM_HELP_ALLSKILLS: &str = "/allskills [角色] — 设置全部已发布技能";
pub(crate) const GM_HELP_REMOVESKILL: &str = "/removeskill <角色> <技能|all> — 移除技能";
pub(crate) const GM_HELP_GIVESTARTERSKILLS: &str = "/givestarterskills <角色> — 补充初始技能";
pub(crate) const GM_SKILL_LEVEL_INVALID: &str = "技能等级必须为 0 至 5。";
pub(crate) fn gm_skill_usage(command: &str) -> String {
    match command {
        "giveskill" => "用法：/giveskill <角色> <技能|all|super> [等级]",
        "allskills" => "用法：/allskills [角色]",
        "removeskill" => "用法：/removeskill <角色> <技能|all>",
        "givestarterskills" => "用法：/givestarterskills <角色>",
        _ => "无效的技能指令。",
    }.into()
}
pub(crate) fn gm_skill_not_skill(skill: &str) -> String { format!("{skill} 不是技能。") }
pub(crate) fn gm_skill_done(command: &str, target: i64) -> String { format!("{command} 已应用于角色 {target}。") }


// Expert-system administration.
pub(crate) const GM_HELP_EXPERT_SYSTEM: &str = "/expertsystem add|remove <typeID> — 添加或移除专家系统";
pub(crate) const GM_EXPERT_SYSTEM_USAGE: &str = "用法：/expertsystem add|remove <typeID>";
/// Invalid GM type ID argument.
pub(crate) fn gm_expert_system_type_id_invalid(text: &str) -> String { format!("专家系统 typeID 必须是整数：{text}") }
/// GM removal result.
pub(crate) fn gm_expert_system_removed(type_id: i32) -> String { format!("已移除专家系统 {type_id}。") }
/// GM addition result.
pub(crate) fn gm_expert_system_added(type_id: i32) -> String { format!("已添加专家系统 {type_id}。") }

// Item administration.
pub(crate) const GM_HELP_SHIP: &str = "/ship <类型> [角色] — 创建飞船";
pub(crate) const GM_HELP_SPAWN: &str = "/spawn <类型> — 停靠时创建飞船并返回物品ID";
pub(crate) const GM_HELP_FIT: &str = "/fit <角色> <类型或物品ID> [数量] — 装配模块";
pub(crate) const GM_HELP_UNLOAD: &str = "/unload me <物品ID或类型ID> — 卸下模块";
pub(crate) const GM_HELP_LOAD: &str = "/load me <类型ID> <数量> — 装填弹药";
pub(crate) const GM_HELP_ONLINE: &str = "/online <飞船ID|me> — 上线模块";
pub(crate) const GM_HELP_ITEMINFO: &str = "/iteminfo <物品ID> — 查看物品";
pub(crate) const GM_HELP_TYPEINFO: &str = "/typeinfo <类型> — 查看类型";
pub(crate) const GM_SHIP_TYPE_REQUIRED: &str = "指定类型不是飞船";
pub(crate) const GM_SHIP_DOCKED_REQUIRED: &str = "目标角色必须停靠在空间站";
pub(crate) const GM_ITEM_NOT_FOUND: &str = "找不到指定物品";
pub(crate) const GM_ITEM_USAGE: &str = "物品命令参数无效";
pub(crate) const GM_ITEM_TARGET_SELF: &str = "此命令仅支持当前角色";
pub(crate) const GM_ITEM_SHIP_USAGE: &str = "用法：/ship <类型> [目标]";
pub(crate) const GM_ITEM_SPAWN_USAGE: &str = "用法：/spawn <类型>";
pub(crate) const GM_ITEM_FIT_USAGE: &str = "用法：/fit <目标> <类型或物品ID> [数量]";
pub(crate) const GM_ITEM_UNLOAD_USAGE: &str = "用法：/unload me <物品ID或类型ID>";
pub(crate) const GM_ITEM_LOAD_USAGE: &str = "用法：/load me <类型ID> <数量>";
pub(crate) const GM_ITEM_ONLINE_USAGE: &str = "用法：/online <飞船ID|me>";
pub(crate) const GM_ITEM_INFO_USAGE: &str = "用法：/iteminfo <物品ID>";
pub(crate) const GM_TYPE_INFO_USAGE: &str = "用法：/typeinfo <类型>";
pub(crate) const GM_FIT_REFUSED: &str = "无法装配：目标、物品或可用槽位无效。";
pub(crate) fn gm_item_created(id: i64, name: &str) -> String { format!("已创建飞船 {name}（{id}）") }
pub(crate) fn gm_item_fitted(count: usize) -> String { format!("已装配 {count} 个模块") }
pub(crate) fn gm_item_unloaded(count: usize) -> String { format!("已卸下 {count} 个模块") }
pub(crate) fn gm_item_loaded(count: usize) -> String { format!("已向 {count} 个兼容模块提交装填请求") }
pub(crate) fn gm_item_online(count: usize) -> String { format!("已上线 {count} 个模块") }
pub(crate) fn gm_item_info(item_id: i64, type_id: i32, owner: i64, location: i64, flag: i32, quantity: i32, singleton: bool) -> String { format!("物品 {item_id}：类型 {type_id}，所有者 {owner}，位置 {location}，标志 {flag}，数量 {quantity}，单例 {singleton}") }
pub(crate) fn gm_type_info(type_id: i32, name: &str, group: i32, category: i32) -> String { format!("类型 {type_id}：{name}，组 {group}，分类 {category}") }
pub(crate) fn gm_type_attribute(id: i32, value: f64) -> String { format!("属性 {id}：{value}") }

// NPC, debris and damage administration.
pub(crate) const GM_HELP_ENTITY: &str = "/entity deploy <数量> <typeID> — 生成 NPC";
pub(crate) const GM_HELP_NPC: &str = "/npc <类型或组> [数量] [距离] — 生成 NPC";
pub(crate) const GM_HELP_NPCCLEAR: &str = "/npcclear [半径|system] — 清除 GM 生成的 NPC";
pub(crate) const GM_HELP_WRECK: &str = "/wreck [类型] [距离] — 生成残骸";
pub(crate) const GM_HELP_CONTAINER: &str = "/container [类型] [距离] — 生成货柜";
pub(crate) const GM_HELP_DMG: &str = "/dmg <shield|armor|hull|all> <百分比> — 设置舰船损伤";
pub(crate) const GM_NPC_USAGE: &str = "用法：/npc <类型或组> [数量] [距离]；/entity deploy <数量> <typeID>";
pub(crate) const GM_NPC_CLEAR_USAGE: &str = "用法：/npcclear [半径|system]";
pub(crate) const GM_DEBRIS_USAGE: &str = "用法：/wreck 或 /container [类型] [距离]";
pub(crate) const GM_DAMAGE_USAGE: &str = "用法：/dmg <shield|armor|hull|all> <0至100的百分比>";
pub(crate) const GM_DAMAGE_SET: &str = "舰船损伤已设置。";
pub(crate) fn gm_npc_spawned(count: usize) -> String { format!("已生成 {count} 个NPC。") }
pub(crate) fn gm_npc_cleared(count: usize) -> String { format!("已移除 {count} 个GM生成的NPC。") }
pub(crate) fn gm_debris_spawned(id: i64) -> String { format!("已生成太空物体 {id}。") }
// Social, progress and session commands.
pub(crate) const GM_HELP_LP: &str = "/lp [角色] — 查看忠诚点";
pub(crate) const GM_HELP_ADDLP: &str = "/addlp <军团> <数量> — 增减忠诚点";
pub(crate) const GM_HELP_SETLP: &str = "/setlp <军团> <数量> — 设置忠诚点";
pub(crate) const GM_SOCIAL_USAGE: &str = "无效的社会指令";
pub(crate) const GM_LP_USAGE: &str = "用法：/lp [角色]";
pub(crate) const GM_LP_CHANGE_USAGE: &str = "用法：/addlp 或 /setlp 军团 数量";
pub(crate) const GM_LP_CORPORATION_REQUIRED: &str = "必须指定军团";
pub(crate) const GM_LP_BALANCE_INVALID: &str = "忠诚点余额不能为负数或超出范围";
pub(crate) fn gm_lp_list(target: i64, rows: &[(i64, i64)]) -> String { let mut s = format!("角色 {target} 的忠诚点："); for (corp, amount) in rows { s.push_str(&format!("\n{corp}: {amount}")); } s }
pub(crate) fn gm_lp_changed(corp: i64, amount: i64) -> String { format!("军团 {corp} 的忠诚点余额：{amount}") }
pub(crate) const GM_HELP_SECSTATUS: &str = "/secstatus [安全等级] [角色] — 查看或设置安全等级";
pub(crate) const GM_HELP_SESSION: &str = "/session — 查看会话属性";
pub(crate) const GM_INFO_USAGE: &str = "无效的信息指令";
pub(crate) const GM_SESSION_USAGE: &str = "用法：/session";
pub(crate) const GM_SECSTATUS_USAGE: &str = "用法：/secstatus [安全等级] [角色]（范围 -10 至 10）；其他玩家的总览将在球体重建时更新";
pub(crate) fn gm_secstatus(character: i64, value: f64) -> String { format!("角色 {character} 的安全等级：{value}") }
pub(crate) fn gm_session(sid: i64, rows: &[(&str, String)]) -> String { let mut s = format!("会话 {sid}："); for (key, value) in rows { s.push_str(&format!("\n{key}: {value}")); } s }
pub(crate) fn gm_site_row_strength(target: &str, kind: &str, what: &str, name: &str, strength: Option<f64>) -> String { match strength { Some(v) => format!("{target} {kind} {what} {name} 信号强度 {v:.1}%"), None => format!("{target} {kind} {what} {name}") } }
pub(crate) const GM_HELP_CORPCREATE: &str = "/corpcreate <名称> <简称> — 创建军团";
pub(crate) const GM_HELP_SETALLIANCE: &str = "/setalliance <名称> <简称> — 创建联盟";
pub(crate) const GM_HELP_JOINALLIANCE: &str = "/joinalliance <联盟> — 加入联盟";
pub(crate) const GM_ORG_CREATE_USAGE: &str = "用法：/corpcreate 或 /setalliance 名称 简称";
pub(crate) const GM_JOINALLIANCE_USAGE: &str = "用法：/joinalliance 联盟";
pub(crate) fn gm_org_created(id: i64) -> String { format!("已创建组织 {id}") }
pub(crate) fn gm_joinalliance(id: i64) -> String { format!("已加入联盟 {id}") }
// Mission administration.
pub(crate) const GM_HELP_MISSIONCOMPLETE: &str = "/missioncomplete [代理人ID|all] — 完成任务目标（分支交互任务除外）";
pub(crate) const GM_MISSIONCOMPLETE_USAGE: &str = "用法：/missioncomplete [代理人ID|all]；all 跳过分支交互任务";
pub(crate) fn gm_missioncomplete(count: usize) -> String { format!("已完成 {count} 个任务目标（不含分支交互任务）") }
pub(crate) const GM_MISSION_INTERACTION_UNSUPPORTED: &str = "分支交互任务需要手动选择，无法通过此命令完成。";
// Wormhole administration.
pub(crate) const GM_WH_USAGE: &str = "用法：/wh status|ensure|clear [here] 或 /wh random [数量] [here]";
pub(crate) const GM_WH_NONE: &str = "当前星系没有可用虫洞。";
pub(crate) const GM_WH_ENSURED: &str = "当前星系已有可用虫洞。";
pub(crate) fn gm_wh_status_row(target: &str, type_id: i32, dest_system: i64, dest_class: i64, mass_used: f64, mass_max: f64) -> String {
    format!("{target} 类型 {type_id} → 星系 {dest_system}（等级 {dest_class}），质量 {mass_used:.0}/{mass_max:.0} 千克")
}
pub(crate) fn gm_wh_created(count: usize) -> String { format!("已创建 {count} 个虫洞连接。") }
pub(crate) fn gm_wh_cleared(count: usize) -> String { format!("已清除 {count} 个虫洞连接。") }
pub(crate) const GM_HELP_SETSTANDING: &str = "/setstanding <所属方> <角色> <声望值> [原因] — 设置声望";
pub(crate) const GM_SETSTANDING_USAGE: &str = "用法：/setstanding 所属方 角色 声望值 [原因]（范围 -10 至 10）";
pub(crate) fn gm_setstanding(owner: i64, character: i64, value: f64) -> String { format!("已设置 {owner} 对 {character} 的声望为 {value}") }
