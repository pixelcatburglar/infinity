//! GM slash-command and per-item dogma override messages.
//! The client slash service consumes these strings; they read no database and
//! implement neither gameplay behavior nor localisation.

/// A command failed without a directly displayable explanation.
pub const GM_FEEDBACK_ERROR: &str = "命令执行失败，请查看服务器日志。";
/// Spawn returns an item ID on the RPC; the conversation labels that ID.
pub fn gm_spawn_feedback(id: i64) -> String { format!("已创建飞船，物品ID：{id}。") }

/// An argument quote was not closed.
pub const GM_ARGS_UNCLOSED_QUOTE: &str = "引号未闭合。";
/// A command requires a selected character.
pub const GM_ARGS_NO_CHARACTER: &str = "当前没有选择角色。";
pub fn gm_args_missing(index: usize) -> String { format!("缺少第 {index} 个参数。") }
pub fn gm_args_integer(text: &str) -> String { format!("无效的整数：{text}") }
pub fn gm_args_number(text: &str) -> String { format!("无效的数值：{text}") }
pub fn gm_args_unknown_target(text: &str) -> String { format!("找不到角色：{text}") }
pub fn gm_args_unknown_owner(text: &str) -> String { format!("找不到所有者：{text}") }
pub fn gm_rows_truncated(count: usize) -> String { format!("另有 {count} 行未显示。") }

// GM slash commands (services/slash.rs)
/// System announcement command help and usage.
pub const GM_HELP_ANNOUNCE: &str = "/announce <message> — 向所有在线角色发送系统通告";
pub const GM_ANNOUNCE_USAGE: &str = "用法：/announce <message>";
pub fn gm_announcement(text: &str) -> String { format!("系统通告：{text}") }
pub fn gm_announce_queued(targets: i64) -> String { format!("已向 {targets} 个在线角色发起通告。") }

/// Teleport command help.
pub const GM_HELP_TELEPORT: &str = "/tr [me] <itemID|characterID|solarSystemID|stationID|celestialID> — 传送";
/// Wormhole command help.
pub const GM_HELP_WORMHOLE: &str = "/wh [status|ensure|random|clear] — 虫洞操作；无参数时前往虫洞";
/// Scan-all command help.
pub const GM_HELP_SCAN_ALL: &str = "/scanall — scan every signature in this system to 100%";
/// Site listing command help.
pub const GM_HELP_SITES: &str = "/sites — 列出当前星系的异常和信号强度";
/// /sigscan is an alias of /scanall.
pub const GM_HELP_SIGSCAN: &str = "/sigscan — 同 /scanall";
/// /sigs is an alias of /sites.
pub const GM_HELP_SIGS: &str = "/sigs — 同 /sites";
/// Overlay-resend command help.
pub const GM_HELP_OVERLAY_REFRESH: &str = "/overlayrefresh — 向本人客户端重发当前星系已加载的扫描覆盖层状态";
/// Overlay-resend usage error.
pub const GM_OVERLAY_REFRESH_USAGE: &str = "用法：/overlayrefresh";
/// Overlay resend requires the caller to be in space.
pub const GM_OVERLAY_SPACE_REQUIRED: &str = "请先驾驶飞船进入太空。";
/// Overlay resend requires the system's scan tracker to be already loaded.
pub const GM_OVERLAY_NOT_READY: &str = "当前星系的扫描跟踪器尚未就绪。";
/// Overlay resend result.
pub const GM_OVERLAY_REFRESHED: &str = "已重发当前星系扫描覆盖层状态。";
/// Create command help.
pub const GM_HELP_CREATE: &str = "/create <typeID|name> [qty] — 创建到机库（停靠时）或货舱；数量支持不区分大小写的 k/m/b/t 后缀，小数结果须为整数";
/// Create-item command help.
pub const GM_HELP_CREATE_ITEM: &str = "/createitem <typeID|name> [qty] — 同 /create；数量支持不区分大小写的 k/m/b/t 后缀，小数结果须为整数";
/// Set ISK command help.
pub const GM_HELP_SET_ISK: &str = "/setisk <amount> — set your wallet";
/// Add ISK command help.
pub const GM_HELP_ADD_ISK: &str = "/addisk <amount> — add ISK to your wallet";
/// Set PLEX command help.
pub const GM_HELP_SET_PLEX: &str = "/setplex <count> — set your PLEX vault";
/// Add PLEX command help.
pub const GM_HELP_ADD_PLEX: &str = "/addplex <count> — add PLEX to your vault";
/// Heal command help.
pub const GM_HELP_HEAL: &str = "/heal — 修复当前舰船及全部已装配模块；太空中同时补满电容并清空热量";
/// Help command help.
pub const GM_HELP_HELP: &str = "/help — this list";
/// Clone grade command help.
pub const GM_HELP_CLONE_GRADE: &str = "/clonegrade <user_id> alpha|omega [duration_seconds [grace_seconds]] — 设置账号克隆等级；省略时长则 Omega 无限期，宽限期默认为 0；时长必须为正数，宽限期不能为负数；Alpha 不接受时长参数。";
/// Clone grade change result.
pub fn clone_grade_changed(user: i64, grade: &str) -> String {
    format!("已将账号 {user} 的克隆等级设置为 {grade}。")
}
/// Teleport command usage error.
pub const GM_TELEPORT_USAGE: &str = "用法：/tr [me] <itemID|characterID|solarSystemID|stationID|celestialID>";
/// Create command usage error.
pub const GM_CREATE_USAGE: &str = "用法：/create <typeID|name> [qty]；数量支持不区分大小写的 k/m/b/t 后缀，小数结果须为整数";
/// Invalid quantity for item creation.
pub fn gm_create_quantity_invalid() -> String { format!("创建数量必须是 1 至 {} 的正整数；支持不区分大小写的 k/m/b/t 后缀，小数换算结果须为整数。", crate::consts::MAX_STACK_QUANTITY) }
/// Set ISK command usage error.
pub const GM_SET_ISK_USAGE: &str = "usage: /setisk <amount>";
/// Add ISK command usage error.
pub const GM_ADD_ISK_USAGE: &str = "usage: /addisk <amount>";
/// Set PLEX command usage error.
pub const GM_SET_PLEX_USAGE: &str = "usage: /setplex <count>";
/// Add PLEX command usage error.
pub const GM_ADD_PLEX_USAGE: &str = "usage: /addplex <count>";
/// Wallet numeric overflow.
pub const GM_AMOUNT_TOO_LARGE: &str = "amount exceeds the wallet limit";
/// No destination for a created item.
pub const GM_NOWHERE_TO_PUT_ITEM: &str = "nowhere to put it";
/// Docked ship and fitted-module repair result.
pub const GM_HEALED_DOCKED: &str = "已修复当前飞船及全部已装配模块；离线模块保持离线。";
/// Complete space repair result.
pub const GM_HEALED_SPACE: &str = "已修复全部已装配模块，并补满护盾、装甲、结构和电容、清空热量；离线与过载状态保持不变。";
/// Space module repair committed but the live target changed.
pub const GM_HEAL_MODULES_ONLY: &str = "已提交全部已装配模块的损伤修复；当前空间目标已变化，舰船资源恢复未应用。";
/// Missing GM role.
pub const GM_ROLE_REQUIRED: &str = "you are not a GM";
/// Unknown item type.
pub fn gm_unknown_type(arg: &str) -> String { format!("no such type: {arg}") }
/// Invalid destination ID.
pub fn gm_invalid_id(dest: &str) -> String { format!("not an ID: {dest}") }
/// Teleport result.
pub fn gm_teleported(id: i64) -> String { format!("teleported to {id}") }
/// Unknown solar system.
pub fn gm_unknown_system(id: i64) -> String { format!("unknown solar system {id}") }
/// Solar-system teleport result.
pub fn gm_teleported_to_system(id: i64) -> String { format!("teleported to system {id}") }
/// Teleport to an item in another system.
pub fn gm_teleported_in_system(id: i64, system: i64) -> String { format!("teleported to {id} in {system}") }
/// Unknown teleport target.
pub fn gm_target_not_found(id: i64) -> String { format!("can't find {id}") }
/// A station destination is not an SDE station.
pub const GM_TRANSPORT_INVALID_STATION: &str = "无效的空间站目的地。";
/// The pilot has no active ship to relocate.
pub const GM_TRANSPORT_NO_SHIP: &str = "没有可移动的当前飞船。";
/// The pilot's location or ship changed while the relocation was being checked.
pub const GM_TRANSPORT_CONFLICT: &str = "角色或飞船状态已变化，传送取消。";
/// An open direct trade must be closed before relocating.
pub const GM_TRANSPORT_TRADE_OPEN: &str = "存在未结束的交易，请先结束交易后再传送。";
/// Created item result.
pub fn gm_created_item(qty: i64, name: &str, type_id: i32) -> String { format!("created {qty} x {name} ({type_id})") }
/// Updated ISK balance, with a client projection when clamped.
pub fn gm_balance(exact: &str, shown: Option<f64>) -> String {
    let mut result = format!("balance {exact} ISK");
    if let Some(shown) = shown { result.push_str(&format!(" (client shows {shown:.0})")); }
    result
}
/// Updated PLEX vault, with a client projection when clamped.
pub fn gm_plex_balance(exact: &str, shown: Option<i64>) -> String {
    let mut result = format!("vault {exact} PLEX");
    if let Some(shown) = shown { result.push_str(&format!(" (client shows {shown})")); }
    result
}
/// One quoted command in the command list.
pub fn gm_command_name(n: &str) -> String { format!("'{n}'") }
/// Remote command list.
pub fn gm_command_list(names: &str) -> String { format!("commands: [{}]", names) }
/// Unknown command and help text.
pub fn gm_unknown_command(other: &str, help: &str) -> String { format!("unknown command /{other}\n{}", help) }

/// Effect diagnostics command help and usage.
pub const GM_HELP_EFFECTS: &str = "/effects unsupported — 列出未支持的效果";
/// Invalid effect diagnostics arguments.
pub const GM_EFFECTS_USAGE: &str = "用法：/effects unsupported";
/// No unsupported effects have been observed.
pub(crate) const GM_EFFECTS_EMPTY: &str = "尚未记录未支持的效果。";
/// Format the observed unsupported effects in snapshot order.
pub fn gm_effects_unsupported(effects: &[(String, i32)]) -> String {
    if effects.is_empty() {
        return GM_EFFECTS_EMPTY.into();
    }
    effects.iter().map(|(name, type_id)| format!("{name}（类型 ID：{type_id}）")).collect::<Vec<_>>().join("\n")
}

// Visual-effect previews (/effect).
/// Preview command help and usage.
pub const GM_HELP_EFFECT: &str = "/effect [list|help|?]|<名称> [me|球ID|target=<球ID>] [moduleType=<类型ID>]|stop|off [名称 [目标]]|stop-all — 向本人客户端发送视觉效果预览";
/// Invalid preview command arguments.
pub const GM_VISUAL_USAGE: &str = "用法：/effect [list|help|?]，/effect <名称> [me|<球ID>|target=<球ID>] [moduleType=<类型ID>]，/effect stop|off [<名称> [<球ID>|target=<球ID>]]，/effect stop-all";
/// An unrecognized effect name, distinct from a refused catalogue row.
pub fn gm_visual_unknown(name: &str) -> String { format!("未知的效果名称：{name}。使用 /effect list 查看目录。") }
/// One row of the preview catalogue.
pub fn gm_visual_effect_row(name: &str, guid: &str, aliases: &str, disposition: &str) -> String {
    format!("{name}（{guid}；别名 {aliases}）：{disposition}")
}

/// The catalogue listing: the usage header followed by every row.
pub fn gm_visual_list(rows: impl Iterator<Item = String>) -> String {
    let mut text = String::from(GM_VISUAL_USAGE);
    for row in rows { text.push('\n'); text.push_str(&row); }
    text
}
/// Catalogue disposition labels: self handle, targeted handle and the two
/// one-shot pulse kinds.
pub const GM_VISUAL_SELF: &str = "自身预览";
pub const GM_VISUAL_TARGETED: &str = "目标预览";
pub const GM_VISUAL_SELF_PULSE: &str = "自身一次性脉冲";
pub const GM_VISUAL_TARGETED_PULSE: &str = "目标一次性脉冲（需要发射类型 moduleType）";
/// Catalogue exclusion classes; the same text also answers a refused request.
pub const GM_VISUAL_EXCLUDED_CLOAK: &str = "已排除：会改动或复用真实的隐身表现，无法安全恢复。";
pub const GM_VISUAL_EXCLUDED_TRIAGE: &str = "已排除：会改变舰船模型共享的围攻模式表现。";
pub const GM_VISUAL_EXCLUDED_TRAVEL: &str = "已排除：会隐藏模型并调用航行/隧道表现，而非独立预览。";
pub const GM_VISUAL_EXCLUDED_EMPTY: &str = "已排除：没有可播放的视觉效果。";
/// A preview start was queued to the caller's observer; `replaced` means an
/// earlier handle of the same effect was stopped in the same batch. A sent
/// pulse ends on client timing and can never be cancelled. `unchecked`
/// appends the zero-link fallback note and `beam_only` the missing own
/// firing-delay note; the two notes are independent.
pub fn gm_visual_effect_sent(name: &str, target: Option<i64>, module_type: Option<i32>, replaced: bool, pulse: bool, unchecked: bool, beam_only: bool) -> String {
    let mut text = if pulse { format!("已发送 {name} 一次性预览请求") } else { format!("已发送 {name} 预览请求") };
    if let Some(target) = target { text += &format!("，目标 {target}"); }
    if let Some(module_type) = module_type { text += &format!("，发射类型 {module_type}"); }
    if replaced { text += "；已同时发送旧预览的停止请求"; }
    text += "。";
    if unchecked { text += GM_VISUAL_MODULE_UNCHECKED; }
    if beam_only { text += GM_VISUAL_BEAM_ONLY; }
    if pulse { text += "已发送的本地脉冲按客户端自身时序结束，发送后不能停止。"; }
    text
}
/// A stop or stop-all request was queued to the owning observer; a stop-all
/// reply also notes that one-shot pulses are outside its reach.
pub fn gm_visual_effect_stops(count: i64, all: bool) -> String {
    let mut text = format!("已发送 {count} 个停止请求。");
    if all { text += "一次性脉冲不受停止请求影响。"; }
    text
}
/// The caller or its ship changed, or another connection owns the records.
pub const GM_VISUAL_CALLER_CHANGED: &str = "角色、飞船或预览记录所属连接已变化，请重试。";
/// The system actor or the caller's observer channel is gone.
pub const GM_VISUAL_SCENE_NOT_READY: &str = "当前星系场景或本人观测通道尚未就绪。";
/// The source ship cannot start a preview right now.
pub const GM_VISUAL_SOURCE_UNAVAILABLE: &str = "当前飞船不能发起预览：不在场景中、正在跃迁、已隐身或不是飞船。";
/// A targeted preview needs a target and nothing is locked.
pub const GM_VISUAL_TARGET_REQUIRED: &str = "该效果需要一个目标：当前没有已锁定的目标，请用 target=<球ID> 指定或先锁定一个目标。";
/// Several locked targets make omission ambiguous.
pub const GM_VISUAL_TARGET_AMBIGUOUS: &str = "该效果需要一个目标：当前有多个已锁定的目标，请用 target=<球ID> 指定一个。";
/// A self preview was given a target.
pub const GM_VISUAL_UNEXPECTED_TARGET: &str = "该效果作用于自身，不接受目标参数。";
/// The named ball is not an eligible target.
pub const GM_VISUAL_INVALID_TARGET: &str = "无效的目标：必须是当前星系中可见的其他舰船或空间站。";
/// An explicit stop target disagrees with the recorded one.
pub const GM_VISUAL_TARGET_MISMATCH: &str = "指定的目标与已发送预览记录的目标不一致。";
/// No loaded type links the effect's GUID, so `moduleType=` is required.
pub const GM_VISUAL_MODULE_REQUIRED: &str = "当前静态数据没有与该效果 GUID 关联的类型；请显式指定提供发射静态数据的 moduleType=<类型ID>。";
/// The ambiguous-candidate header; the following lines list candidate IDs.
pub const GM_VISUAL_MODULE_AMBIGUOUS: &str = "当前静态数据关联了多个候选类型；请从以下候选中显式指定 moduleType=<类型ID>：";
/// The refusal when no type links the effect GUID and the explicit fallback
/// is not a known module type owning an attribute row.
pub const GM_VISUAL_MODULE_INVALID: &str = "没有效果类型关联时，moduleType 必须是已知模块类型，且至少拥有一条自有静态属性记录。";
/// The explicit type is not one of the effect's linked candidate types.
pub const GM_VISUAL_MODULE_MISMATCH: &str = "指定的 moduleType 不是该效果的静态候选类型。";
/// Send note: the zero-link fallback could not be checked against the effect.
pub const GM_VISUAL_MODULE_UNCHECKED: &str = "当前静态数据没有该效果的类型关联，无法校验所选类型与效果的匹配关系。";
/// Send note: the type owns no `damageDelayDuration` row, so the client plays
/// beam-only and skips the target impact.
pub const GM_VISUAL_BEAM_ONLY: &str = "该类型未自带 damageDelayDuration；客户端按仅光束（beam-only）路径处理并跳过目标命中表现。";
/// The ambiguous-candidate refusal: the header followed by one decimal
/// candidate type ID per line, in the order supplied.
pub fn gm_visual_module_candidates(candidate_ids: &[i32]) -> String {
    use std::fmt::Write;
    let mut text = String::from(GM_VISUAL_MODULE_AMBIGUOUS);
    for id in candidate_ids { let _ = write!(text, "\n{id}"); }
    text
}
/// A module type argument was given to a non-weapon effect.
pub const GM_VISUAL_MODULE_UNEXPECTED: &str = "该效果不接受 moduleType 参数。";
/// A pulse cannot be cancelled after being sent.
pub const GM_VISUAL_ONE_SHOT_STOP: &str = "一次性脉冲按客户端自身时序结束，发送后不能停止。";

/// Native turret accuracy diagnostic.
pub const GM_HELP_HITCHANCE: &str = "/hitchance <目标ID> — 查看炮台命中率及计算输入";
pub const GM_HITCHANCE_USAGE: &str = "用法：/hitchance <目标ID>";
pub const GM_HITCHANCE_UNDOCK: &str = "请先驾驶飞船离开空间站。";
pub const GM_HITCHANCE_MISSING_BALL: &str = "当前飞船或目标不在当前星系中。";
pub const GM_HITCHANCE_EMPTY: &str = "当前飞船没有装配炮台。";
pub fn gm_hitchance_row(item: i64, type_id: i32, center: f64, surface: f64, speed: f64, angular: f64, angular_squared: f64, tracking: f64, opt_sig: f64, target_sig: f64, optimal: f64, falloff: f64, chance: f64) -> String {
    format!("炮台物品 {item}，类型 {type_id}：中心距离={center:.17e}，表面距离={surface:.17e}，相对速度={speed:.17e}，角速度={angular:.17e}，角速度平方={angular_squared:.17e}，追踪速度={tracking:.17e}，炮台信号半径={opt_sig:.17e}，目标信号半径={target_sig:.17e}，最佳射程={optimal:.17e}，失准范围={falloff:.17e}，命中率={chance:.17e}")
}

// GM per-item dogma overrides.
pub const GM_HELP_DOGMA: &str = "/dogma <ship|me|物品ID|flag:N> <属性ID|名称> [[=] 数值|reset|infinity]；/dogma <目标> list；me 为当前飞船，等号两侧空格可省略";
pub const GM_DOGMA_USAGE: &str = "用法：/dogma <ship|me|物品ID|flag:N> <属性ID|名称> [[=] 数值|reset|infinity]，或 /dogma <目标> list；me 为当前飞船，等号两侧空格可省略。";
pub const GM_DOGMA_NO_SHIP: &str = "没有当前飞船。";
pub const GM_DOGMA_SINGLETON: &str = "只能修改单件物品，请使用已组装的单件物品。";
pub const GM_DOGMA_FORBIDDEN_TARGET: &str = "不能修改弹药、角色、建筑或 NPC 的属性。";
pub const GM_DOGMA_RUNTIME: &str = "此属性由运行状态管理；护盾、装甲和结构请使用 /heal，在线状态请使用模块在线开关。";
pub const GM_DOGMA_CHARACTER: &str = "角色属性不能通过 /dogma 修改。";
pub const GM_DOGMA_ACTIVE_CYCLE: &str = "运行中的模块不能修改周期属性；请先关闭模块。";
pub const GM_DOGMA_KINEMATIC_INVALID: &str = "运动属性数值无效：质量和灵活性必须为正数，最大速度不能为负数，且数值必须在可表示范围内。";
pub const GM_DOGMA_VISIBILITY: &str = "模块或船体原本没有的属性不会显示在装配窗口和 HUD 最大值中。";
pub const GM_DOGMA_BASE_ONLY: &str = "目标不在当前装配中；仅显示基础值，实际有效值取决于其装配和驾驶员。";
pub const GM_DOGMA_EMPTY: &str = "没有属性覆盖。";
pub const GM_DOGMA_UNLIMITED: &str = "无限货舱：服务器不设体积配额，装货时按实算。";
pub const GM_DOGMA_UNLIMITED_ATTR: &str = "infinity 只能用于飞船的容量 (capacity) 属性。";
pub const GM_DOGMA_UNLIMITED_TARGET: &str = "只有已组装的整艘飞船才能开启无限货舱。";
pub fn gm_dogma_invalid_target(s: &str) -> String { format!("无效的目标：{s}") }
pub fn gm_dogma_attribute(s: &str) -> String { format!("未知属性：{s}") }
pub fn gm_dogma_ambiguous(s: &str, ids: &str) -> String { format!("属性名称 {s} 不唯一，请使用 ID：{ids}") }
pub fn gm_dogma_result(item: i64, attr: &str, id: i32, old_base: f64, new_base: f64, effective: Option<(f64, f64)>, overridden: bool, unlimited: bool) -> String {
    let effective = effective.map(|(old, new)| format!("；有效值 {old} → {new}")).unwrap_or_default();
    let mode = if unlimited { "；无限货舱" } else { "" };
    format!("物品 {item} 属性 {attr} ({id})：基础值 {old_base} → {new_base}{effective}{mode}；覆盖：{}", if overridden { "是" } else { "否" })
}
pub fn gm_dogma_noop(item: i64, id: i32) -> String { format!("物品 {item} 的属性 {id} 没有覆盖，无需重置。") }
pub fn gm_dogma_list_item(flag: i32, name: &str, item: i64) -> String { format!("位置 {flag}，{name}，物品 {item}") }
pub fn gm_dogma_list_entry(id: i32, value: f64, unlimited: bool) -> String { format!("  属性 {id}: {value}{}", if unlimited { "（无限）" } else { "" }) }

// Lua administration.
pub const LUA_HELP: &str = "用法：/lua reload 或 /lua list";
pub const LUA_RELOADED: &str = "脚本已重新加载。";
pub fn lua_reload_failed(error: &str) -> String { format!("脚本重新加载失败：{error}") }
pub fn lua_list(list: &str) -> String { format!("Lua 脚本：\n{list}") }

// Skill administration.
pub const GM_HELP_GIVESKILL: &str = "/giveskill <角色> <技能|all|super> [等级] — 设置技能等级";
pub const GM_HELP_ALLSKILLS: &str = "/allskills [角色] — 设置全部已发布技能";
pub const GM_HELP_REMOVESKILL: &str = "/removeskill <角色> <技能|all> — 移除技能";
pub const GM_HELP_GIVESTARTERSKILLS: &str = "/givestarterskills <角色> — 补充初始技能";
/// Starter-skill reset help; the destructive reset is irreversible.
pub const GM_HELP_BACK_IN_TIME: &str = "/backintime [me|角色ID|角色名] — 不可逆重置：将角色技能恢复为种族初始技能，删除其余技能并清空技能队列和自由技能点";
pub const GM_SKILL_LEVEL_INVALID: &str = "技能等级必须为 0 至 5。";
pub fn gm_skill_usage(command: &str) -> String {
    match command {
        "giveskill" => "用法：/giveskill <角色> <技能|all|super> [等级]",
        "allskills" => "用法：/allskills [角色]",
        "removeskill" => "用法：/removeskill <角色> <技能|all>",
        "givestarterskills" => "用法：/givestarterskills <角色>",
        "backintime" => "用法：/backintime [me|角色ID|角色名]；不可逆重置为种族初始技能",
        _ => "无效的技能指令。",
    }.into()
}
pub fn gm_skill_not_skill(skill: &str) -> String { format!("{skill} 不是技能。") }
pub const GM_HELP_SETATTR: &str = "/setattr <数值> [me|角色ID|角色名] — 设置五项基础属性";
pub const GM_SETATTR_USAGE: &str = "用法：/setattr <数值> [me|角色ID|角色名]；数值不能为负数，四舍五入后不能超过 i32::MAX。";
pub fn gm_setattr_done(target: i64, value: i32) -> String { format!("已将角色 {target} 的五项基础属性设置为 {value}。") }
pub fn gm_skill_done(command: &str, target: i64) -> String { format!("{command} 已应用于角色 {target}。") }


// Expert-system administration.
pub const GM_HELP_EXPERT_SYSTEM: &str = "/expertsystem add|remove <typeID> — 添加或移除专家系统";
pub const GM_EXPERT_SYSTEM_USAGE: &str = "用法：/expertsystem add|remove <typeID>";
/// Invalid GM type ID argument.
pub fn gm_expert_system_type_id_invalid(text: &str) -> String { format!("专家系统 typeID 必须是整数：{text}") }
/// GM removal result.
pub fn gm_expert_system_removed(type_id: i32) -> String { format!("已移除专家系统 {type_id}。") }
/// GM addition result.
pub fn gm_expert_system_added(type_id: i32) -> String { format!("已添加专家系统 {type_id}。") }

// Item administration.
pub const GM_HELP_SHIP: &str = "/ship <类型> [角色] — 创建飞船";
pub const GM_HELP_SPAWN: &str = "/spawn <类型> — 停靠时创建飞船并返回物品ID";
pub const GM_HELP_FIT: &str = "/fit <角色> <类型或物品ID> [数量] — 装配模块";
pub const GM_HELP_UNLOAD: &str = "/unload me <物品ID或类型ID> — 卸下模块";
pub const GM_HELP_UNFIT: &str = "/unfit me <物品ID|类型ID|名称|all> — 卸下当前飞船的普通槽位模块";
pub const GM_HELP_LOAD: &str = "/load me <类型ID> <数量> — 装填弹药";
pub const GM_HELP_ONLINE: &str = "/online <飞船ID|me> — 上线模块";
pub const GM_HELP_ITEMINFO: &str = "/iteminfo <物品ID> — 查看物品";
pub const GM_HELP_TYPEINFO: &str = "/typeinfo <类型> — 查看类型";
pub const GM_SHIP_TYPE_REQUIRED: &str = "指定类型不是飞船";
pub const GM_SHIP_DOCKED_REQUIRED: &str = "目标角色必须停靠在空间站";
pub const GM_ITEM_NOT_FOUND: &str = "找不到指定物品";
pub const GM_ITEM_USAGE: &str = "物品命令参数无效";
pub const GM_ITEM_TARGET_SELF: &str = "此命令仅支持当前角色";
pub const GM_ITEM_SHIP_USAGE: &str = "用法：/ship <类型> [目标]";
pub const GM_ITEM_SPAWN_USAGE: &str = "用法：/spawn <类型>";
pub const GM_ITEM_FIT_USAGE: &str = "用法：/fit <目标> <类型或物品ID> [数量]";
pub const GM_ITEM_UNLOAD_USAGE: &str = "用法：/unload me <物品ID或类型ID>";
pub const GM_ITEM_UNFIT_USAGE: &str = "用法：/unfit me <物品ID|类型ID|名称|all>";
pub const GM_UNFIT_RESTRICTED: &str = "无法卸下改装件或子系统。";
pub const GM_UNFIT_EMPTY: &str = "没有可卸的模块。";
pub const GM_ITEM_LOAD_USAGE: &str = "用法：/load me <类型ID> <数量>";
pub const GM_ITEM_ONLINE_USAGE: &str = "用法：/online <飞船ID|me>";
pub const GM_ITEM_INFO_USAGE: &str = "用法：/iteminfo <物品ID>";
pub const GM_TYPE_INFO_USAGE: &str = "用法：/typeinfo <类型>";
pub const GM_FIT_REFUSED: &str = "无法装配：目标、物品或可用槽位无效。";
pub fn gm_item_created(id: i64, name: &str) -> String { format!("已创建飞船 {name}（{id}）") }
pub fn gm_item_fitted(count: usize) -> String { format!("已装配 {count} 个模块") }
pub fn gm_item_unloaded(count: usize) -> String { format!("已卸下 {count} 个模块") }
pub fn gm_item_loaded(count: usize) -> String { format!("已向 {count} 个兼容模块提交装填请求") }
pub fn gm_item_online(count: usize) -> String { format!("已上线 {count} 个模块") }
pub fn gm_item_info(item_id: i64, type_id: i32, owner: i64, location: i64, flag: i32, quantity: i64, singleton: bool) -> String { format!("物品 {item_id}：类型 {type_id}，所有者 {owner}，位置 {location}，标志 {flag}，数量 {quantity}，单例 {singleton}") }
pub fn gm_type_info(type_id: i32, name: &str, group: i32, category: i32) -> String { format!("类型 {type_id}：{name}，组 {group}，分类 {category}") }
pub fn gm_type_attribute(id: i32, value: f64) -> String { format!("属性 {id}：{value}") }

// Ship dirt administration.
/// Dirt command help: a bare command or `status` shows the stored value;
/// `clear` and ratio `0` write the current time; ratio `1` saturates at the
/// finite timestamp `0`.
pub const GM_HELP_DIRT: &str = "/dirt [<0..1>|status|clear] [shipID] — 查看或设置舰船污渍程度；无参数等同 status，省略 shipID 时使用当前飞船";
pub const GM_DIRT_USAGE: &str = "用法：/dirt [<0..1>|status|clear] [shipID]";
/// Live-publication outcome texts shared by `gm_dirt_changed` and
/// `gm_killmarks_changed`: the slim was updated, the hull had no running
/// actor, or its incarnation changed after commit.
pub const GM_DIRT_PUBLISHED: &str = "已同步到太空中的球体";
pub const GM_DIRT_NOT_LOADED: &str = "已保存，该舰船下次真实装载时生效";
pub const GM_DIRT_STALE: &str = "已保存，但太空目标在提交后已变化，将于下次真实装载时恢复";
/// Dirt status reply; `state` is `(timestamp, age_ms, attained_level, attained_ratio)`.
pub fn gm_dirt_status(ship: i64, state: Option<(i64, i64, f64, f64)>) -> String {
    match state {
        Some((stamp, age_ms, level, ratio)) => format!("舰船 {ship} 的污渍：时间戳 {stamp}，船龄 {age_ms} 毫秒，实际程度 {level:.4}（比例 {ratio:.4}）。"),
        None => format!("舰船 {ship} 尚无污渍记录。"),
    }
}
/// Dirt write reply; `publication` is one of the `GM_DIRT_*` outcome texts.
pub fn gm_dirt_changed(ship: i64, requested: f64, stamp: i64, level: f64, attained: f64, saturated: bool, publication: &str) -> String {
    let outcome = if saturated { format!("已达上限，实际比例 {attained:.4}") } else { format!("实际比例 {attained:.4}") };
    format!("舰船 {ship} 的污渍已设置：请求比例 {requested}，时间戳 {stamp}，实际程度 {level:.4}（{outcome}）；{publication}")
}

// Ship kill-count administration.
/// Killmark command help: `/killmark` and `/killmarks` share one
/// implementation; a bare command or `status` shows the stored count,
/// `<count>` sets the absolute value, `clear` writes 0; an omitted shipID
/// uses the caller's current ship.
pub const GM_HELP_KILLMARKS: &str = "/killmark|/killmarks [<count>|status|clear] [shipID] — 查看或设置舰船击杀计数；无参数等同 status，<count> 为绝对设置，clear 等同 0，省略 shipID 时使用当前飞船";
/// Killmark command usage error.
pub const GM_KILLMARKS_USAGE: &str = "用法：/killmark 或 /killmarks [<count>|status|clear] [shipID]";
/// Kill count that is negative, fractional or beyond the i64 range.
pub const GM_KILLMARKS_COUNT_INVALID: &str = "击杀计数必须是 0 至 9223372036854775807 的整数。";
/// Killmark status reply; the full count is reported, the hangar model caps at 999.
pub fn gm_killmarks_status(ship: i64, count: i64) -> String {
    format!("舰船 {ship} 的击杀计数：{count}（机库模型最多显示 999）。")
}
/// Killmark write reply; `publication` is one of the shared `GM_DIRT_*`
/// outcome texts.
pub fn gm_killmarks_changed(ship: i64, count: i64, publication: &str) -> String {
    format!("舰船 {ship} 的击杀计数已设置为 {count}（机库模型最多显示 999）；{publication}。")
}

// NPC, debris and damage administration.
pub const GM_HELP_ENTITY: &str = "/entity deploy <数量> <typeID> — 生成 NPC";
pub const GM_HELP_NPC: &str = "/npc <类型或组> [数量] [距离] — 生成 NPC";
pub const GM_HELP_NPCCLEAR: &str = "/npcclear [半径|system] — 清除 GM 生成的 NPC";
pub const GM_HELP_WRECK: &str = "/wreck [类型] [距离] — 生成残骸";
pub const GM_HELP_CONTAINER: &str = "/container [类型] [距离] — 生成货柜";
/// System-wide debris cleanup command help.
pub const GM_HELP_SYSJUNKCLEAR: &str = "/sysjunkclear — 永久删除当前星系内全部残骸和普通货柜及其内容物";
/// Nearby debris cleanup command help.
pub const GM_HELP_TESTCLEAR: &str = "/testclear — 永久删除本人飞船周围 20km 内的残骸和普通货柜及其内容物";
pub const GM_HELP_DMG: &str = "/dmg <shield|armor|hull|all> <百分比> — 设置舰船损伤";
pub const GM_NPC_USAGE: &str = "用法：/npc <类型或组> [数量] [距离]；/entity deploy <数量> <typeID>";
pub const GM_NPC_CLEAR_USAGE: &str = "用法：/npcclear [半径|system]";
pub const GM_DEBRIS_USAGE: &str = "用法：/wreck 或 /container [类型] [距离]";
/// System-wide debris cleanup usage.
pub const GM_SYSJUNKCLEAR_USAGE: &str = "用法：/sysjunkclear（无参数；需要有效的当前星系）";
/// Nearby debris cleanup usage.
pub const GM_TESTCLEAR_USAGE: &str = "用法：/testclear（无参数；需在太空中驾驶飞船）";
/// The batch was refused; it intentionally names no item.
pub const GM_DEBRIS_REFUSED: &str = "所选残骸或货柜中有受保护的物品，或其内容刚刚发生变化；未删除任何对象。";
/// No valid solar system in the session for the system-wide cleanup.
pub const GM_DEBRIS_NO_SYSTEM: &str = "当前会话没有有效的星系位置，无法清理残骸或货柜。";
pub const GM_DAMAGE_USAGE: &str = "用法：/dmg <shield|armor|hull|all> <0至100的百分比>";
pub const GM_DAMAGE_SET: &str = "舰船损伤已设置。";
/// Self-destruction command help.
pub const GM_HELP_SUICIDE: &str = "/suicide — 自毁本人当前太空舰船或太空舱，沿用正常损失处理";
/// Self-destruction usage error.
pub const GM_SUICIDE_USAGE: &str = "用法：/suicide（无参数；仅限本人当前太空舰船或太空舱）";
/// The caller's session or current ship is not eligible for self-destruction.
pub const GM_SUICIDE_UNAVAILABLE: &str = "当前会话或太空舰船不可用于自毁；请确认已出站且未在换船或损失处理中。";
/// Self-destruction result.
pub fn gm_suicide_completed(ship: i64) -> String { format!("舰船 {ship} 的自毁处理已完成。") }
pub fn gm_npc_spawned(count: usize) -> String { format!("已生成 {count} 个NPC。") }
pub fn gm_npc_cleared(count: usize) -> String { format!("已移除 {count} 个GM生成的NPC。") }
pub fn gm_debris_spawned(id: i64) -> String { format!("已生成太空物体 {id}。") }
/// Debris cleanup result with confirmed space removal.
pub fn gm_debris_cleared(roots: i64, items: i64) -> String { format!("已永久删除 {roots} 个残骸/货柜，共 {items} 个物品（含其全部内容）。") }
/// Debris cleanup result where the rows are deleted but space removal is unconfirmed.
pub fn gm_debris_cleared_unconfirmed(roots: i64, items: i64) -> String { format!("数据库已永久删除 {roots} 个残骸/货柜，共 {items} 个物品；现场对象移除未确认。") }

// NPC target-ignore administration.
/// NPC-ignore command help.
pub const GM_HELP_INVU: &str = "/invu [on|off|status] — 查看或设置当前角色的 NPC 目标忽略；无参数切换";
/// NPC-ignore command usage.
pub const GM_INVU_USAGE: &str = "用法：/invu [on|off|status]";
/// Read-only status reply.
pub fn gm_invu_status(ignored: bool) -> String {
    format!("NPC 目标忽略当前{}。", if ignored { "开启" } else { "关闭" })
}
/// The flag committed and every live scene confirmed it.
pub const GM_INVU_ENABLED: &str = "已开启 NPC 目标忽略：当前飞船及以本角色为所有者的无人机不再是 NPC 的合法目标。";
/// The flag cleared and every live scene confirmed it.
pub const GM_INVU_DISABLED: &str = "已关闭 NPC 目标忽略：恢复本角色的正常 NPC 选敌。";
/// The flag committed, but a live scene was already gone or stopped and did
/// not confirm; its next real load picks the persisted value up.
pub fn gm_invu_saved(ignored: bool) -> String {
    format!("NPC 目标忽略已{}并保存；部分场景已离开或停止，将在实际重载时恢复。", if ignored { "开启" } else { "关闭" })
}

// Crimewatch administration.
/// Crimewatch command help.
pub const GM_HELP_CWATCH: &str = "/cwatch [status|clear|safety full|partial|none|naughty|weapon|pvp|npc|criminal|suspect|disapproval on|off|<时长>] — 查看或设置本人的战斗计时器与安全设置";
/// Naughty command help.
pub const GM_HELP_NAUGHTY: &str = "/naughty — 刷新本人的武器与 PvP 计时器；高安星系同时设置罪犯标记并请求 CONCORD 响应";
/// Crimewatch command usage error.
pub const GM_CWATCH_USAGE: &str = "用法：/cwatch [status|clear|safety full|partial|none|naughty|weapon|pvp|npc|criminal|suspect|disapproval on|off|<时长>]；时长为十进制数加可选 ms/s/m/h 后缀，无后缀按秒";
/// Naughty command usage error.
pub const GM_NAUGHTY_USAGE: &str = "用法：/naughty（无参数）";
/// Unparseable timer duration argument.
pub fn gm_cwatch_duration_invalid(text: &str) -> String { format!("无效的时长：{text}；时长为十进制数加可选 ms/s/m/h 后缀，无后缀按秒，结果须为整数毫秒。") }
/// Safety level names shared by the status and safety-saved replies.
fn safety_name(level: i64) -> &'static str {
    match level {
        0 => "关闭",
        1 => "部分",
        2 => "完全",
        _ => "未知",
    }
}
/// `/cwatch` status reply; `deadlines` holds the six timer deadlines in
/// weapons, PvP, NPC, criminal, suspect and disapproval order, `None` for off.
pub fn gm_cwatch_status(character: i64, deadlines: [Option<i64>; 6], safety: i64) -> String {
    const NAMES: [&str; 6] = ["武器", "PvP", "NPC", "罪犯", "嫌犯", "不认可"];
    let mut text = format!("角色 {character} 的 Crimewatch 状态：");
    for (name, deadline) in NAMES.iter().zip(deadlines) {
        match deadline {
            Some(until) => text.push_str(&format!("\n{name}计时器：截止 {until}")),
            None => {
                text.push('\n');
                text.push_str(name);
                text.push_str("计时器：已关闭");
            }
        }
    }
    text.push_str(&format!("\n安全设置：{}（{safety}）", safety_name(safety)));
    text
}
/// `/cwatch safety` result: the server pushes nothing and enforces nothing
/// extra, so the client refreshes its display at the next login, ship swap or
/// system change.
pub fn gm_cwatch_safety_saved(level: i64) -> String {
    format!("安全设置已保存为 {}；服务器不主动推送，也不新增任何安全执法，客户端将在下次登录、换船或切换星系时刷新显示。", safety_name(level))
}
/// `/cwatch clear` result: timers off, a still-pending CONCORD response
/// cancelled; safety, security status, engagements and kill rights are kept.
pub const GM_CWATCH_CLEARED: &str = "已关闭全部六项 Crimewatch 计时器并取消尚未执行的 CONCORD 响应；已被接受的损失不受影响。安全设置、安全等级、交战与击杀权保持不变。";
/// `/naughty` in high-sec: a response was already queued or was queued now.
pub const GM_CWATCH_NAUGHTY_QUEUED: &str = "已刷新武器与 PvP 计时器并设置罪犯标记；CONCORD 响应已排队。";
/// `/naughty` in high-sec with the response suppressed by NPC target ignore or
/// disabled CONCORD responses; the criminal flag is still set.
pub const GM_CWATCH_NAUGHTY_IGNORED: &str = "已刷新武器与 PvP 计时器并设置罪犯标记；未请求 CONCORD 响应（NPC 目标忽略已开启或 CONCORD 响应已禁用）。";
/// `/naughty` outside high-sec: weapons and PvP timers only, no new criminal
/// flag and an existing one stays.
pub const GM_CWATCH_NAUGHTY_NOT_HIGHSEC: &str = "当前不在高安星系：仅刷新武器与 PvP 计时器；未新增罪犯标记，已有罪犯标记保持不变。";

// Social, progress and session commands.
pub const GM_HELP_LP: &str = "/lp [角色] — 查看忠诚点";
pub const GM_HELP_ADDLP: &str = "/addlp <军团> <数量> — 增减忠诚点";
pub const GM_HELP_SETLP: &str = "/setlp <军团> <数量> — 设置忠诚点";
pub const GM_SOCIAL_USAGE: &str = "无效的社会指令";
pub const GM_LP_USAGE: &str = "用法：/lp [角色]";
pub const GM_LP_CHANGE_USAGE: &str = "用法：/addlp 或 /setlp 军团 数量";
pub const GM_LP_CORPORATION_REQUIRED: &str = "必须指定军团";
pub const GM_LP_BALANCE_INVALID: &str = "忠诚点余额不能为负数或超出范围";
pub fn gm_lp_list(target: i64, rows: &[(i64, i64)]) -> String { let mut s = format!("角色 {target} 的忠诚点："); for (corp, amount) in rows { s.push_str(&format!("\n{corp}: {amount}")); } s }
pub fn gm_lp_changed(corp: i64, amount: i64) -> String { format!("军团 {corp} 的忠诚点余额：{amount}") }
/// EverMarks query command help.
pub const GM_HELP_EVERMARKS: &str = "/evermarks — 查看当前角色的 EverMarks";
/// Corporation EverMarks query command help.
pub const GM_HELP_CORPEVERMARKS: &str = "/corpevermarks — 查看当前所属军团的 EverMarks";
/// Personal EverMarks add command help.
pub const GM_HELP_ADDEVERMARKS: &str = "/addevermarks <数量> — 增减当前角色的 EverMarks";
/// Personal EverMarks set command help.
pub const GM_HELP_SETEVERMARKS: &str = "/setevermarks <数量> — 设置当前角色的 EverMarks";
/// Corporation EverMarks add command help.
pub const GM_HELP_ADDCORPEVERMARKS: &str = "/addcorpevermarks <数量> — 增减当前所属军团的 EverMarks";
/// Corporation EverMarks set command help.
pub const GM_HELP_SETCORPEVERMARKS: &str = "/setcorpevermarks <数量> — 设置当前所属军团的 EverMarks";
/// EverMarks query usage error.
pub const GM_EVERMARKS_USAGE: &str = "用法：/evermarks 或 /corpevermarks";
/// EverMarks add usage error.
pub const GM_EVERMARKS_ADD_USAGE: &str = "用法：/addevermarks 或 /addcorpevermarks <数量>";
/// EverMarks set usage error.
pub const GM_EVERMARKS_SET_USAGE: &str = "用法：/setevermarks 或 /setcorpevermarks <数量>";
/// The caller's character has no valid current corporation.
pub const GM_LP_CURRENT_CORPORATION_REQUIRED: &str = "当前角色没有有效的所属军团。";
/// The caller's corporation changed while the organization locks were taken.
pub const GM_LP_MEMBERSHIP_CHANGED: &str = "角色所属军团已变化，请重试。";
/// The loyalty balance changed between the read and the guarded write.
pub const GM_LP_BALANCE_CHANGED: &str = "忠诚点余额已变化，请重试。";
/// Exact EverMarks balance of a character or corporation owner.
pub fn gm_evermarks_balance(owner: i64, corporation: bool, amount: i64) -> String {
    format!("{} {} 的 EverMarks 余额：{amount}", if corporation { "军团" } else { "角色" }, owner)
}
pub const GM_HELP_SECSTATUS: &str = "/secstatus [安全等级] [角色] — 查看或设置安全等级";
pub const GM_HELP_SESSION: &str = "/session — 查看会话属性";
pub const GM_INFO_USAGE: &str = "无效的信息指令";
pub const GM_SESSION_USAGE: &str = "用法：/session";
pub const GM_SECSTATUS_USAGE: &str = "用法：/secstatus [安全等级] [角色]（范围 -10 至 10）；其他玩家的总览将在球体重建时更新";
pub fn gm_secstatus(character: i64, value: f64) -> String { format!("角色 {character} 的安全等级：{value}") }
pub fn gm_session(sid: i64, rows: &[(&str, String)]) -> String { let mut s = format!("会话 {sid}："); for (key, value) in rows { s.push_str(&format!("\n{key}: {value}")); } s }

// Read-only queries: location, online pilots, wallet, hangar and server build.
pub const GM_HELP_WHERE: &str = "/where — 查看当前位置";
pub const GM_HELP_WHO: &str = "/who — 查看在线角色";
pub const GM_HELP_WALLET: &str = "/wallet — 查看钱包余额";
pub const GM_HELP_HANGAR: &str = "/hangar — 查看当前空间站的舰船机库";
pub const GM_WHERE_USAGE: &str = "用法：/where";
pub const GM_WHO_USAGE: &str = "用法：/who";
pub const GM_WALLET_USAGE: &str = "用法：/wallet";
pub const GM_HANGAR_USAGE: &str = "用法：/hangar";
pub const GM_HELP_VERSION: &str = "/version — 查看服务器版本和提交号";
pub const GM_VERSION_USAGE: &str = "用法：/version";
pub fn gm_version(version: &str, commit: &str) -> String {
    format!("服务器版本：{version}，提交：{commit}")
}
/// Shared location text for /where and /who: the system name with its real ID
/// when the SDE provides a name, otherwise the ID alone; a docked pilot also
/// sees the station ID.
fn write_location(out: &mut String, system: Option<i64>, system_name: Option<&str>, station: Option<i64>) {
    use std::fmt::Write;
    match (system, system_name) {
        (Some(id), Some(name)) => { let _ = write!(out, "星系 {name}（{id}）"); }
        (Some(id), None) => { let _ = write!(out, "星系 {id}"); }
        (None, Some(name)) => { let _ = write!(out, "星系 {name}"); }
        (None, None) => out.push_str("星系未知"),
    }
    if let Some(station) = station { let _ = write!(out, "，空间站 {station}"); }
}
/// The caller's location from the live session; docked pilots keep the system.
pub fn gm_where(system: Option<i64>, system_name: Option<&str>, station: Option<i64>) -> String {
    let mut text = String::from("当前位置：");
    write_location(&mut text, system, system_name, station);
    text
}
/// Online-pilot listing header; the count covers the full snapshot, not shown rows.
pub fn gm_who_header(total: i64) -> String { format!("当前在线角色共 {total} 名：") }
/// One online pilot; the name is omitted when no character name was found.
pub fn gm_who_row(character: i64, name: Option<&str>, system: Option<i64>, system_name: Option<&str>, station: Option<i64>) -> String {
    use std::fmt::Write;
    let mut text = format!("角色 {character}");
    if let Some(name) = name { let _ = write!(text, "（{name}）"); }
    text.push('，');
    write_location(&mut text, system, system_name, station);
    text
}
/// Exact wallet balances; the caps only bound what the client can display.
pub fn gm_wallet(isk: &str, plex: &str, isk_display_cap: &str, plex_display_cap: i64) -> String {
    format!("钱包余额：{isk} ISK，{plex} PLEX。客户端显示上限：{isk_display_cap} ISK，{plex_display_cap} PLEX。")
}
/// Hangar header: the hangar row count and total hulls are reported separately.
pub fn gm_hangar_header(station: i64, rows: i64, ships: i128) -> String {
    format!("空间站 {station} 舰船机库：{rows} 条记录，共 {ships} 艘舰船：")
}
/// One hangar ship: custom or type name when known, pack state, exact quantity
/// and the active-ship mark.
pub fn gm_hangar_row(item: i64, type_id: i32, name: Option<&str>, quantity: i64, singleton: bool, current: bool) -> String {
    use std::fmt::Write;
    let mut text = format!("物品 {item}，类型 {type_id}");
    if let Some(name) = name { let _ = write!(text, "，{name}"); }
    let _ = write!(text, "，{}，数量 {quantity}", if singleton { "已组装" } else { "已打包" });
    if current { text.push_str("，当前飞船"); }
    text
}
/// Wormhole description in a GM site list.
pub fn gm_wormhole_site(type_id: i32, dest_system: i64, dest_class: i64) -> String {
    format!("虫洞 {type_id} → 星系 {dest_system}（等级 {dest_class}）")
}
/// Data-site content class in a GM site list.
pub const GM_SITE_DATA: &str = "数据";
/// Relic-site content class in a GM site list.
pub const GM_SITE_RELIC: &str = "遗迹";
/// Gas-site content class in a GM site list.
pub const GM_SITE_GAS: &str = "气云";
/// Ore-anomaly content class in a GM site list.
pub const GM_SITE_ORE: &str = "矿石";
/// Ice-anomaly content class in a GM site list.
pub const GM_SITE_ICE: &str = "冰带";
/// Gravimetric site with no mined field in a GM site list.
pub const GM_SITE_MINING: &str = "采矿";
/// Combat-site content class in a GM site list.
pub const GM_SITE_COMBAT: &str = "战斗";
/// Unrecognized content class in a GM site list.
pub const GM_SITE_UNKNOWN: &str = "未知类别";
/// Anomaly kind in a GM site list.
pub const GM_SITE_ANOMALY: &str = "异常";
/// Signature kind in a GM site list.
pub const GM_SITE_SIGNATURE: &str = "信号";
/// One GM site row: the scan target first (the list sorts by it), then the real
/// site ID, kind, content class and name. A signature also shows its scan
/// strength: how readily probes resolve the site, not the caller's progress.
pub fn gm_site_row_strength(site_id: i64, target: &str, kind: &str, what: &str, name: &str, strength: Option<f64>) -> String {
    match strength {
        Some(v) => format!("{target} 站点ID {site_id} {kind} {what} {name} 信号强度 {v:.1}%"),
        None => format!("{target} 站点ID {site_id} {kind} {what} {name}"),
    }
}
pub const GM_HELP_CORPCREATE: &str = "/corpcreate <名称> <简称> — 创建军团";
pub const GM_HELP_SETALLIANCE: &str = "/setalliance <名称> <简称> — 创建联盟";
pub const GM_HELP_JOINALLIANCE: &str = "/joinalliance <联盟> — 加入联盟";
pub const GM_ORG_CREATE_USAGE: &str = "用法：/corpcreate 或 /setalliance 名称 简称";
pub const GM_JOINALLIANCE_USAGE: &str = "用法：/joinalliance 联盟";
pub fn gm_org_created(id: i64) -> String { format!("已创建组织 {id}") }
pub fn gm_joinalliance(id: i64) -> String { format!("已加入联盟 {id}") }
// Mission administration.
pub const GM_HELP_MISSIONCOMPLETE: &str = "/missioncomplete [代理人ID|all] — 完成任务目标（分支交互任务除外）";
pub const GM_MISSIONCOMPLETE_USAGE: &str = "用法：/missioncomplete [代理人ID|all]；all 跳过分支交互任务";
pub fn gm_missioncomplete(count: usize) -> String { format!("已完成 {count} 个任务目标（不含分支交互任务）") }
pub const GM_MISSION_INTERACTION_UNSUPPORTED: &str = "分支交互任务需要手动选择，无法通过此命令完成。";
// Wormhole administration.
pub const GM_WH_USAGE: &str = "用法：/wh status|ensure|clear [here] 或 /wh random [数量] [here]";
pub const GM_WH_NONE: &str = "当前星系没有可用虫洞。";
pub const GM_WH_ENSURED: &str = "当前星系已有可用虫洞。";
pub fn gm_wh_status_row(target: &str, type_id: i32, dest_system: i64, dest_class: i64, mass_used: f64, mass_max: f64) -> String {
    format!("{target} 类型 {type_id} → 星系 {dest_system}（等级 {dest_class}），质量 {mass_used:.0}/{mass_max:.0} 千克")
}
pub fn gm_wh_created(count: usize) -> String { format!("已创建 {count} 个虫洞连接。") }
pub fn gm_wh_cleared(count: usize) -> String { format!("已清除 {count} 个虫洞连接。") }
pub const GM_HELP_SETSTANDING: &str = "/setstanding <所属方> <角色> <声望值> [原因] — 设置声望并记录历史";
pub const GM_SETSTANDING_USAGE: &str = "用法：/setstanding 所属方 角色 声望值 [原因]（范围 -10 至 10）";
pub fn gm_setstanding(owner: i64, character: i64, value: f64) -> String { format!("已设置 {owner} 对 {character} 的声望为 {value}") }
/// Bulk agent standing commands; the two names share one implementation.
pub const GM_HELP_MAXAGENTSTANDINGS: &str = "/maxagentstandings [角色ID|完整名称] — 将目标对所有未删除代理人的军团及所属势力的声望设为 +10";
pub const GM_HELP_FULLSTANDINGS: &str = "/fullstandings [角色ID|完整名称] — /maxagentstandings 的别名";
/// Reply of /maxagentstandings and /fullstandings: agent corporation and
/// faction owner counts, the owners actually raised and those already at +10.
pub fn gm_agent_standings(character: i64, corporations: i64, factions: i64, changed: i64, already_full: i64) -> String {
    format!("角色 {character}：军团 {corporations} 个，势力 {factions} 个，已提升 {changed} 个，已满 {already_full} 个")
}

// SKIN administration.
/// All-SKINs command help and usage.
pub const GM_HELP_ALLSKINS: &str = "/allskins [me|角色ID|\"完整角色名\"] — 向目标永久授予全部普通 SKIN";
pub const GM_ALLSKINS_USAGE: &str = "用法：/allskins [me|角色ID|\"完整角色名\"]";
/// Bulk grant result.
pub fn gm_all_skins_done(character: i64, changed: i64, already_permanent: i64) -> String {
    format!("角色 {character}：已永久授予或升级 {changed} 个 SKIN，已永久拥有 {already_permanent} 个（跳过）。")
}

// Solar-system preloading.
/// Current-system preload command help.
pub const GM_HELP_LOADSYS: &str = "/loadsys — 预加载当前公共星系及星门直接目的地（停靠可用）";
/// Full public-set preload command help.
pub const GM_HELP_LOADALLSYS: &str = "/loadallsys — 预加载完整公共星系集合（含无星门星系）；需要在配置中显式开启性能测试";
/// Idle-system unload command help and usage.
pub const GM_HELP_UNLOADALLSYS: &str = "/unloadallsys — 无参数，仅卸载已运行的空闲星系；需要在配置中显式开启性能测试";
pub const GM_UNLOADALLSYS_USAGE: &str = "用法：/unloadallsys";
/// Full-system performance commands require explicit operator authorization.
pub const GM_PERFORMANCE_TESTING_REQUIRED: &str = "请先在配置中显式开启性能测试开关。";
/// Unload batch summary, including skipped and unprocessed targets.
pub fn gm_system_unload(total: i64, unloaded: i64, busy: i64, changed: i64, failed: i64, unprocessed: i64) -> String {
    format!("星系卸载结果：目标 {total}，已卸载 {unloaded}，忙碌跳过 {busy}，原实例变化或消失 {changed}，失败 {failed}，未处理 {unprocessed}。")
}
/// Usage errors: neither command accepts arguments.
pub const GM_LOADSYS_USAGE: &str = "用法：/loadsys";
pub const GM_LOADALLSYS_USAGE: &str = "用法：/loadallsys";
/// The session has no usable public static-map location for `/loadsys`.
pub const GM_PRELOAD_NO_PUBLIC_SYSTEM: &str = "当前会话没有公共静态地图中的星系位置，无法预加载。";
/// Preload batch summary. The batch is complete only when nothing failed and
/// nothing remains unprocessed. `newly_loaded` counts targets confirmed loaded
/// when their turn ran, including systems a concurrent request finished
/// loading first. It is never the number of systems this command itself
/// started.
pub fn gm_system_preload(total: i64, originally_loaded: i64, newly_loaded: i64, failed: i64, unprocessed: i64) -> String {
    let state = if failed == 0 && unprocessed == 0 { "星系预加载批次已完成" } else { "星系预加载批次未全部成功" };
    format!("{state}：目标 {total}，开批时已加载 {originally_loaded}，本批期间变为已加载 {newly_loaded}，失败 {failed}，未处理 {unprocessed}。“本批期间变为已加载”包括其他并发请求先加载完成的星系。")
}
// Planned shutdown administration (crates/server/src/shutdown.rs).
/// Shutdown command help and usage.
pub const GM_HELP_SHUTDOWN: &str = "/shutdown <分钟>|cancel|status — 安排、取消或查询计划停服";
pub const GM_SHUTDOWN_USAGE: &str = "用法：/shutdown <分钟>|cancel|status";
/// Schedule or replace reply.
pub fn gm_shutdown_scheduled(minutes: i64) -> String { format!("已计划 {minutes} 分钟后停服。") }
/// No plan exists.
pub const GM_SHUTDOWN_NOT_SCHEDULED: &str = "当前没有计划停服。";
/// Cancel reply.
pub const GM_SHUTDOWN_CANCELLED: &str = "已取消计划停服。";
/// The plan is already due or triggered; nothing can change it any more.
pub const GM_SHUTDOWN_DUE: &str = "停服已进入执行阶段，无法再更改。";
/// Status reply while the plan is triggering.
pub const GM_SHUTDOWN_TRIGGERED: &str = "停服已开始执行。";
/// Status reply with the remaining countdown.
pub fn gm_shutdown_remaining(seconds: u64) -> String { format!("停服倒计时：剩余 {seconds} 秒。") }
