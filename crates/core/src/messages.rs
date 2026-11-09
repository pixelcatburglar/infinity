//! Server-authored player-visible text and client message-catalogue keys.
//! The Infinity client displays Chinese text in Simplified Chinese; message
//! names are keys in its catalogue, including `eveexceptions.UserError` and
//! the client's slash service. This module reads no database and does not
//! implement localisation, data-derived names, or protocol identifiers.

pub mod account;
pub mod brawl;
pub mod market;
pub mod wallets;
pub mod world_tasks;
pub mod ship_adaption;
pub mod void_space;
pub mod random_jump;
pub mod shipcaster;
pub mod pvp_filaments;
pub mod platform;
pub mod support;
pub mod season;
pub mod bluebird;
pub mod gacha;
pub mod raffles;
/// Client market broker quote mismatch catalogue key.
pub const MSG_MKT_BROKERS_FEE_UNEXPECTED: &str = "MktBrokersFeeUnexpected2";
/// Client multi-order item limit catalogue key.
pub const MSG_TOO_MANY_ITEMS_FOR_ORDER: &str = "TooManyItemsForOrder";
/// Client corporation access denial catalogue key.
pub const MSG_CRP_ACCESS_DENIED: &str = "CrpAccessDenied";
/// Client inventory full-space and restricted-destination message keys.
pub const MSG_NOT_ENOUGH_CARGO_SPACE: &str = "NotEnoughCargoSpace";
pub const MSG_NOT_ENOUGH_DRONE_BAY_SPACE: &str = "NotEnoughDroneBaySpace";
pub const MSG_NOT_ENOUGH_FIGHTER_BAY_SPACE: &str = "NotEnoughFighterBaySpace";
pub const MSG_NOT_ENOUGH_SPECIAL_BAY_SPACE: &str = "NotEnoughSpecialBaySpace";
pub const MSG_CANNOT_STORE_DESTINATION_RESTRICTED: &str = "CannotStoreDestinationRestricted";
pub mod inventory;
pub mod encoded_items;
pub mod access_groups;
pub mod bookmarks;
pub mod bounty;
pub mod chat;
pub mod clones;
pub mod combat;
pub mod contacts;
pub mod corporations;
pub mod corps;
pub mod crates;
pub mod drones;
pub mod effects;
pub mod dynamic_items;
pub mod exploration;
pub mod faction_warfare;
pub mod ess;
pub mod fitting;
pub mod fighters;
pub mod warp_vector;
pub mod super_weapon;
pub mod fleets;
pub mod gateway;
pub mod gm;
pub mod expert_systems;
pub mod heat;
pub mod industry;
pub mod mining;
pub mod lookup;
pub mod missions;
pub mod npe;
pub mod newbie;
pub mod mail;
pub mod map;
pub mod plex_vault;
pub mod planetary;
pub mod repair;
pub mod scheduler;
pub mod settings_sharing;
pub mod lua;
pub mod loyalty;
pub mod standings;
pub mod trade;
pub mod skills;
pub mod skill_plans;
pub mod store;
pub mod structures;
pub mod subscriptions;
pub mod wars;
pub mod ship_skins;
pub mod ship;

/// Invalid structure deployment or lifecycle command.
pub const STRUCTURE_OPERATION_INVALID: &str = "无效的结构操作请求。";

// Client moderation UserError catalogue keys.
pub const MSG_BANNED_WORDS_DOWN: &str = "BannedWordsDown";
pub const MSG_LOCATION_NAME_INVALID_BANNED_WORD: &str = "LocationNameInvalidBannedWord";

// Client fitting and online UserError catalogue keys.
pub const MSG_FITTING_HAS_SKILL_PREREQUISITES: &str = "FittingHasSkillPrerequisites";
pub const MSG_ONLINE_HAS_SKILL_PREREQUISITES: &str = "OnlineHasSkillPrerequisites";
pub const MSG_NOT_ENOUGH_CPU: &str = "NotEnoughCpu";
pub const MSG_NOT_ENOUGH_POWER: &str = "NotEnoughPower";
pub const MSG_CANT_FIT_TOO_MANY_BY_GROUP: &str = "CantFitTooManyByGroup";
pub const MSG_CANT_FIT_TOO_MANY_BY_TYPE: &str = "CantFitTooManyByType";
pub const MSG_CANNOT_ONLINE_REACHED_MAX_GROUP_ONLINE: &str = "CannotOnlineReachedMaxGroupOnline";
pub const MSG_EFFECT_CROWDED_OUT: &str = "EffectCrowdedOut";
pub const MSG_MODULE_NOT_POWERED: &str = "ModuleNotPowered";
pub const MSG_NO_SUITABLE_SLOTS_FOR_THIS_SUBSYSTEM: &str = "NoSuitableSlotsForThisSubSystem";
pub const MSG_CANNOT_FIT_SUBSYSTEM_NOT_SHIP: &str = "CannotFitSubSystemNotShip";

// Client skill injector UserError catalogue key.
pub const MSG_ALREADY_INJECTED_TODAY: &str = "AlreadyInjectedToday";

/// Proxy usurp reason from `carbon/common/script/net/machoNet.py`.
pub const SESSION_USURPED: &str = "The user's connection has been usurped on the proxy";
/// Generic connection refusal mapped by `carbon/common/script/net/ExceptionMappingGPCS.py:16`.
pub const ACL_NOTACCEPTING: &str = "ACL_NOTACCEPTING";

// Alliances (services/alliances.rs)
/// Alliance action requires a player-corporation CEO.
pub const PLAYER_CORP_CEO_REQUIRED: &str = "只有玩家军团的 CEO 才能这样做。";
/// Joining an alliance when the corporation already has one.
pub const CORP_ALREADY_IN_ALLIANCE: &str = "你的军团已经在一个联盟中。";
/// Alliance creation name length validation.
pub const ALLIANCE_NAME_LENGTH_INVALID: &str = "联盟名称长度必须为 4 到 50 个字符。";
/// Alliance short-name validation.
pub const ALLIANCE_SHORT_NAME_INVALID: &str = "联盟简称必须为 2 到 5 个字母、数字、点或短横线。";
/// Alliance uniqueness validation.
pub const ALLIANCE_NAME_OR_SHORT_NAME_TAKEN: &str = "该联盟名称或简称已被占用。";
/// Alliance join target corporation missing.
pub const CORP_NOT_FOUND: &str = "该军团已不存在。";
/// The target corporation already belongs to an alliance.
pub const TARGET_CORP_ALREADY_IN_ALLIANCE: &str = "该军团已经在一个联盟中。";
/// Choosing alliance executor.
pub const CORP_CEO_REQUIRED_TO_CHOOSE_EXECUTOR: &str = "只有军团 CEO 才能选择支持的执行军团。";
/// Executor/removal target is not an alliance member.
pub const CORP_NOT_ALLIANCE_MEMBER: &str = "该军团不是联盟成员。";
/// Alliance lookup/application operation.
pub const ALLIANCE_NOT_FOUND: &str = "该联盟已不存在。";
/// Editing alliance information.
pub const EXECUTOR_CORP_DIRECTOR_REQUIRED_TO_EDIT_ALLIANCE: &str = "只有执行军团的董事才能修改联盟信息。";
/// Alliance application missing.
pub const ALLIANCE_APPLICATION_NOT_FOUND: &str = "该申请已不存在。";
/// Alliance application status validation.
pub const ALLIANCE_APPLICATION_STATUS_INVALID: &str = "无效的申请状态。";
/// Alliance member removal permission.
pub const NOT_AUTHORIZED_TO_REMOVE_CORP_FROM_ALLIANCE: &str = "你无法将该军团移出联盟。";
/// Alliance relationship update permission.
pub const EXECUTOR_CORP_DIRECTOR_REQUIRED_TO_SET_RELATIONSHIP: &str = "只有执行军团的董事才能设置联盟关系。";

// Bills (services/bills.rs)
/// Bill is paid or missing.
pub const BILL_ALREADY_PAID_OR_MISSING: &str = "该账单已支付或不存在。";
/// Corporate bill payment permission.
pub const BILL_PAYMENT_PERMISSION_REQUIRED: &str = "你没有支付该账单的职权。";
/// Personal bill ownership check.
pub const BILL_NOT_OWNED_BY_CHARACTER: &str = "这不是你的账单。";
/// Bill autopay settings permission.
pub const ACCOUNTANT_REQUIRED_FOR_AUTOPAY_SETTINGS: &str = "只有会计才能修改自动付款设置。";

// Bookmarks (services/bookmarks.rs)
/// Bookmark target unavailable.
pub const BOOKMARK_TARGET_MOVED_OR_MISSING: &str = "目标已不在原处。";
/// Bookmarking requires a fully scanned signature.
pub const BOOKMARK_REQUIRES_FULL_SCAN: &str = "请先将信号扫描到 100% 再保存位置。";

// Calendar (services/calendar.rs)
/// Calendar event missing.
pub const CALENDAR_EVENT_NOT_FOUND: &str = "该活动已不存在。";
/// Calendar event edit permission.
pub const CALENDAR_EVENT_EDIT_NOT_AUTHORIZED: &str = "你无法修改该活动。";
/// Corporation calendar event creation permission.
pub const PLAYER_CORP_DIRECTOR_REQUIRED_FOR_CORP_EVENT: &str = "只有玩家军团的董事才能创建军团活动。";
/// Alliance calendar events unavailable.
pub const ALLIANCE_EVENTS_UNSUPPORTED: &str = "暂不支持联盟活动。";

// Character selection (services/charsel.rs)
/// A generated character name.
pub fn random_character_name(first: &str, last: &str, suffix: usize) -> String { format!("{} {} {}", first, last, suffix) }
/// Generated character first name.
pub const RANDOM_FIRST_AREN: &str = "Aren";
/// Generated character first name.
pub const RANDOM_FIRST_KAEL: &str = "Kael";
/// Generated character first name.
pub const RANDOM_FIRST_SORA: &str = "Sora";
/// Generated character first name.
pub const RANDOM_FIRST_VEYRA: &str = "Veyra";
/// Generated character first name.
pub const RANDOM_FIRST_TOMA: &str = "Toma";
/// Generated character first name.
pub const RANDOM_FIRST_ILSA: &str = "Ilsa";
/// Generated character first name.
pub const RANDOM_FIRST_DARO: &str = "Daro";
/// Generated character first name.
pub const RANDOM_FIRST_MIRA: &str = "Mira";
/// Generated character first name.
pub const RANDOM_FIRST_LUKAS: &str = "Lukas";
/// Generated character first name.
pub const RANDOM_FIRST_NADIA: &str = "Nadia";
/// Generated character first name.
pub const RANDOM_FIRST_JOREN: &str = "Joren";
/// Generated character first name.
pub const RANDOM_FIRST_TESSA: &str = "Tessa";
/// Generated character surname.
pub const RANDOM_LAST_VALE: &str = "Vale";
/// Generated character surname.
pub const RANDOM_LAST_OKARO: &str = "Okaro";
/// Generated character surname.
pub const RANDOM_LAST_THORNE: &str = "Thorne";
/// Generated character surname.
pub const RANDOM_LAST_KASSEN: &str = "Kassen";
/// Generated character surname.
pub const RANDOM_LAST_RUUN: &str = "Ruun";
/// Generated character surname.
pub const RANDOM_LAST_HALVIK: &str = "Halvik";
/// Generated character surname.
pub const RANDOM_LAST_MOREAU: &str = "Moreau";
/// Generated character surname.
pub const RANDOM_LAST_SANDOR: &str = "Sandor";
/// Generated character surname.
pub const RANDOM_LAST_IKONEN: &str = "Ikonen";
/// Generated character surname.
pub const RANDOM_LAST_DRAKE: &str = "Drake";
/// Generated character surname.
pub const RANDOM_LAST_ARVEL: &str = "Arvel";
/// Generated character surname.
pub const RANDOM_LAST_CASTOR: &str = "Castor";

// Clones (services/clones.rs)
/// Duplicate jump clone at station.
pub const JUMP_CLONE_ALREADY_AT_STATION: &str = "你在这个空间站已经有一个远距克隆。";
/// Jump clone destination missing.
pub const JUMP_CLONE_NOT_FOUND: &str = "该远距克隆已不在原处。";
/// Structure clone installation unavailable.
pub const CLONE_STRUCTURE_INSTALL_UNSUPPORTED: &str = "暂不支持建筑。";

// Combat (services/combat.rs)
/// Charge quantity unavailable.
pub const INSUFFICIENT_AMMUNITION: &str = "弹药数量不足。";
/// Mining module target validation.
pub const MINING_MODULE_TARGET_INVALID: &str = "只能对小行星、冰矿或气云使用采矿设备。";
/// Target distance exceeds lock range.
pub fn target_lock_range(gap: f64, range: f64) -> String { format!("目标距离 {:.1} 公里，超出了你的锁定距离（{:.1} 公里）。", gap, range) }
/// Maximum simultaneous targets.
pub fn target_limit(max_targets: usize) -> String { format!("你最多只能同时锁定 {} 个目标。", max_targets) }
/// Target distance exceeds mining equipment range.
pub fn mining_range(range: f64) -> String { format!("目标超出了采矿设备的范围（{:.1} 公里）。", range) }

// Contracts (services/contracts.rs)
/// Contract type required.
pub const CONTRACT_TYPE_REQUIRED: &str = "请选择合同类型。";
/// Contract monetary values must not be negative.
pub const CONTRACT_AMOUNT_NEGATIVE: &str = "金额不能为负数。";
/// Contract cannot be assigned to issuer.
pub const CONTRACT_SELF_ASSIGNMENT_FORBIDDEN: &str = "你不能将合同指定给自己。";
/// Auction requires offered items.
pub const AUCTION_REQUIRES_ITEMS: &str = "拍卖需要有待售物品。";
/// Auction buyout must not undercut start price.
pub const BUYOUT_BELOW_START_PRICE: &str = "一口价不能低于起拍价。";
/// Courier contract requires items.
pub const COURIER_REQUIRES_ITEMS: &str = "运输合同需要有待运送的物品。";
/// Item exchange needs offer or request.
pub const ITEM_EXCHANGE_REQUIRES_OFFER_OR_REQUEST: &str = "物品交换需要提供或索取物品。";
/// Contract item limit exceeded.
pub const CONTRACT_ITEM_LIMIT_EXCEEDED: &str = "一份合同中的物品过多。";
/// Contract item missing.
pub const CONTRACT_ITEM_MISSING: &str = "其中一件物品已不在原处。";
/// Contract items must be in start-station hangar.
pub const CONTRACT_ITEM_MUST_BE_IN_START_HANGAR: &str = "只有起始空间站物品机库中的物品才能放入合同。";
/// Courier package cannot be traded.
pub const COURIER_PACKAGE_NOT_TRADEABLE: &str = "运输包裹不能交易。";
/// Contract item quantity insufficient.
pub const CONTRACT_ITEM_QUANTITY_INSUFFICIENT: &str = "其中一件物品的数量不足。";
/// Issuer cannot accept own contract.
pub const CONTRACT_SELF_ACCEPTANCE_FORBIDDEN: &str = "你不能接受自己的合同。";
/// Contract is assigned to another party.
pub const CONTRACT_ASSIGNED_TO_OTHER: &str = "该合同已指定给其他人。";
/// Required contract items missing at station.
pub const CONTRACT_REQUIRED_ITEMS_MISSING: &str = "你在合同所在空间站的物品机库中没有全部所需物品。";
/// Only auctions accept bids.
pub const AUCTION_ONLY_BIDDABLE: &str = "拍卖只能出价竞拍。";
/// Courier delivery requires carrier.
pub const COURIER_DELIVERY_REQUIRES_ACCEPTOR: &str = "只有承运人才能交付这份合同。";
/// Courier package must be in destination hangar.
pub const COURIER_PACKAGE_MUST_BE_AT_DESTINATION: &str = "包裹必须位于目的地空间站的机库中。";
/// Courier cannot currently be failed.
pub const COURIER_CANNOT_BE_FAILED_NOW: &str = "现在无法将这份合同判定为失败。";
/// Only assignee may reject contract.
pub const CONTRACT_ASSIGNEE_REQUIRED_TO_REJECT: &str = "只有合同的指定对象才能拒绝合同。";
/// Contract status transition invalid.
pub const CONTRACT_STATUS_TRANSITION_INVALID: &str = "合同不能被设置为该状态。";
/// Only issuer may delete unfinished contract.
pub const CONTRACT_ISSUER_REQUIRED_TO_DELETE: &str = "只有发布者才能删除未完成的合同。";
/// Auction with bids cannot be deleted.
pub const BIDDED_AUCTION_CANNOT_BE_DELETED: &str = "已有出价的拍卖不能删除。";
/// Issuer cannot bid own auction.
pub const AUCTION_SELF_BID_FORBIDDEN: &str = "你不能对自己的拍卖出价。";
/// Auction assigned to another party.
pub const AUCTION_ASSIGNED_TO_OTHER: &str = "该拍卖已指定给其他人。";
/// Bid must exceed current and starting bids.
pub const BID_MUST_EXCEED_CURRENT_AND_START: &str = "你的出价必须高于当前出价和起拍价。";
/// Item no longer at original location.
pub const ITEM_NOT_AT_ORIGINAL_LOCATION: &str = "该物品已不在原处。";
/// Stack cannot be split this way.
pub const STACK_CANNOT_BE_SPLIT: &str = "无法这样拆分该堆叠。";
/// No contract creation request was supplied.
pub const CONTRACT_CREATION_REQUEST_MISSING: &str = "没有要创建的合同。";
/// Default name of a courier contract package.
pub fn contract_package_name(id: i64) -> String { format!("contract {id}") }
/// Outstanding contract limit.
pub fn contract_limit(open: i64, cap: i64) -> String { format!("你有 {open} 份未完成的合同，你的合同技能最多允许 {cap} 份。") }

// Corporations (services/corps.rs)
/// Corporation creation requires docking.
pub const STATION_DOCK_REQUIRED_TO_CREATE_CORP: &str = "你必须停靠在空间站内才能创建军团。";
/// Corporation name length validation.
pub const CORP_NAME_LENGTH_INVALID: &str = "军团名称长度必须为 4 到 50 个字符。";
/// Corporation short-name validation.
pub const CORP_SHORT_NAME_INVALID: &str = "军团代号必须为 2 到 5 个字母、数字、点或短横线。";
/// Corporation creation skill requirement.
pub const CORPORATION_MANAGEMENT_SKILL_REQUIRED: &str = "创建军团需要军团管理技能。";
/// CEO must resign or close current corporation first.
pub const ALREADY_CORP_CEO_MUST_RESIGN_OR_CLOSE: &str = "你已是 CEO，请先辞职或关闭你的军团。";
/// Corporation name or ticker is already taken.
pub const CORP_NAME_OR_SHORT_NAME_TAKEN: &str = "该军团名称或代号已被占用。";
/// NPC corporation cannot be left this way.
pub const CANNOT_LEAVE_NPC_CORP: &str = "不能用这种方式退出 NPC 军团。";
/// Corporation application already processed.
pub const CORP_APPLICATION_ALREADY_PROCESSED: &str = "该申请已处理完毕。";
/// Application does not belong to this character.
pub const CORP_APPLICATION_NOT_OWNED: &str = "这不是你的申请。";
/// Personnel manager permission required to handle applications.
pub const PERSONNEL_MANAGER_REQUIRED_FOR_APPLICATION: &str = "只有军团的人事经理才能处理申请。";
/// CEO must leave own corporation before joining another.
pub const CEO_MUST_LEAVE_CORP_BEFORE_JOINING: &str = "CEO 必须先离开自己的军团才能加入其他军团。";
/// Member role modification is unauthorized.
pub const CANNOT_MODIFY_MEMBER_ROLES: &str = "你无法修改该成员的职权。";
/// Director permission required to edit titles.
pub const DIRECTOR_REQUIRED_TO_EDIT_TITLES: &str = "只有董事才能修改头衔。";
/// Director permission required to delete titles.
pub const DIRECTOR_REQUIRED_TO_DELETE_TITLES: &str = "只有董事才能删除头衔。";
/// Member is ineligible to become CEO.
pub const MEMBER_CANNOT_BECOME_CEO: &str = "该成员无法成为 CEO。";
/// NPC corporation does not accept applications.
pub const NPC_CORP_DOES_NOT_ACCEPT_APPLICATIONS: &str = "NPC 军团不接受申请。";
/// Corporation is not currently accepting applications.
pub const CORP_NOT_RECRUITING: &str = "该军团目前不接受申请。";
/// Personnel manager permission required to invite.
pub const PERSONNEL_MANAGER_REQUIRED_TO_INVITE: &str = "只有人事经理才能发出邀请。";
/// Director permission required to edit corporation information.
pub const DIRECTOR_REQUIRED_TO_EDIT_CORP_INFO: &str = "只有董事才能修改军团信息。";

// Corporation wallet (services/corpwallet.rs)
/// Transfer recipient must be player character or corporation.
pub const TRANSFER_RECIPIENT_NOT_PLAYER: &str = "只能向玩家角色或玩家军团转账。";
/// Wallet transfer validation failed.
pub const TRANSFER_INVALID: &str = "无效的转账。";
/// Department wallet withdrawal permission.
pub const DEPARTMENT_WALLET_WITHDRAW_UNAUTHORIZED: &str = "你没有从该部门钱包取款的职权。";

// Crimewatch (services/crimewatch.rs)
/// Criminal ship-destruction notification.
pub const CONCORD_DESTROYED_SHIP_FOR_CRIME: &str = "统合部因你的犯罪行为摧毁了你的舰船。";

// Exploration (services/exploration.rs)
/// Probe launcher has insufficient probes.
pub const INSUFFICIENT_PROBE_LAUNCHER_PROBES: &str = "探针发射器中的探针数量不足。";
/// Scan result no longer exists.
pub const SCAN_RESULT_GONE: &str = "扫描结果已经消失。";
/// Warp requires fully scanned signature.
pub const SIGNATURE_REQUIRES_FULL_SCAN_FOR_WARP: &str = "你需要先将该信号扫描到 100% 才能跃迁过去。";
/// Wormhole collapsed.
pub const WORMHOLE_COLLAPSED: &str = "这个虫洞已经坍塌了。";
/// Ship is polarized by wormhole.
pub const WORMHOLE_POLARIZED_FOR_SHIP: &str = "这个虫洞对你的舰船处于极化状态，暂时无法再次跃迁。";
/// A ship exceeds the wormhole mass limit.
pub fn wormhole_mass_limit(mass: f64) -> String { format!("你的舰船太重了，无法通过这个虫洞（最大 {:.0} 吨）。", mass) }
/// GM command needs a pilot in space.
pub const GM_UNDOCK_FIRST: &str = "undock first";
/// GM wormhole lookup found none.
pub const GM_NO_WORMHOLES: &str = "no wormholes anywhere yet";
/// GM wormhole lookup found a recently closed wormhole.
pub const GM_WORMHOLE_CLOSED: &str = "that wormhole just closed";
/// GM wormhole lookup found a non-wormhole site.
pub const GM_NOT_WORMHOLE: &str = "not a wormhole";
/// GM wormhole teleport result.
pub fn gm_wormhole_result(target: &str, type_id: i32, system: i64, dest_system: i64, dest_class: i64) -> String { format!("wormhole {} (type {}) in {} -> system {} (class {})", target, type_id, system, dest_system, dest_class) }
/// GM scan-all result.
pub fn gm_sites_scanned(count: usize) -> String { format!("{} sites scanned", count) }
/// Wormhole description in a GM site list.
pub fn gm_wormhole_site(type_id: i32, dest_system: i64, dest_class: i64) -> String { format!("wormhole {} -> {} (class {})", type_id, dest_system, dest_class) }
/// Data site label in a GM site list.
pub const GM_SITE_DATA: &str = "data";
/// Relic site label in a GM site list.
pub const GM_SITE_RELIC: &str = "relic";
/// Combat site label in a GM site list.
pub const GM_SITE_COMBAT: &str = "combat";
/// Anomaly label in a GM site list.
pub const GM_SITE_ANOMALY: &str = "anomaly";
/// Signature label in a GM site list.
pub const GM_SITE_SIGNATURE: &str = "signature";

// Fitting (services/fitting.rs)
/// Insufficient item quantity.
pub const INSUFFICIENT_ITEM_QUANTITY: &str = "该物品的数量不足。";

// Hacking (services/hacking.rs)
/// Analyzer target validation.
pub const ANALYZER_TARGET_MUST_BE_DATA_OR_RELIC: &str = "分析仪只能用于数据和遗迹地点的容器。";
/// Container already hacked.
pub const HACK_CONTAINER_ALREADY_HACKED: &str = "这个容器已经被破解了。";
/// Relic analyzer requirement.
pub const RELIC_CONTAINER_REQUIRES_RELIC_ANALYZER: &str = "这是遗迹容器，需要使用遗迹分析仪。";
/// Data analyzer requirement.
pub const DATA_CONTAINER_REQUIRES_DATA_ANALYZER: &str = "这是数据容器，需要使用数据分析仪。";
/// Analyzer target is out of range.
pub fn analyzer_range(gap: f64, range: f64) -> String { format!("目标太远了（{:.1} 公里），分析仪的范围是 {:.1} 公里。", gap, range) }

// Industry (services/industry.rs)
/// Facility lacks industry services.
pub const NO_INDUSTRIAL_SERVICES: &str = "该空间站没有工业服务。";
/// Project exceeds duration limit.
pub const INDUSTRY_PROJECT_TOO_LONG: &str = "该项目耗时过长，请减少流程数。";
/// Blueprint is already in use.
pub const BLUEPRINT_IN_USE: &str = "该蓝图正在被一个项目使用。";
/// Activity unavailable at facility.
pub const INDUSTRY_ACTIVITY_UNAVAILABLE: &str = "该空间站不提供这项活动。";
/// Blueprint must be at facility.
pub const BLUEPRINT_MUST_BE_AT_FACILITY: &str = "蓝图必须位于该设施中。";
/// Blueprint cannot be used for activity.
pub const BLUEPRINT_ACTIVITY_INVALID: &str = "该蓝图不能用于这项活动。";
/// Run count exceeds blueprint limit.
pub const BLUEPRINT_RUN_COUNT_EXCEEDED: &str = "流程数超出了蓝图允许的范围。";
/// Facility outside remote industry range.
pub const FACILITY_OUTSIDE_REMOTE_INDUSTRY_RANGE: &str = "该设施超出了你的远程工业范围。";
/// Project already delivered.
pub const INDUSTRY_PROJECT_ALREADY_DELIVERED: &str = "该项目已经交付。";
/// Project not complete.
pub const INDUSTRY_PROJECT_NOT_COMPLETE: &str = "该项目尚未完成。";
/// No project to install.
pub const NO_PROJECT_TO_INSTALL: &str = "没有要安装的项目。";
/// Project cannot be cancelled.
pub const INDUSTRY_PROJECT_CANNOT_CANCEL: &str = "该项目无法取消。";
/// Skill required by an industry project.
pub fn required_skill_message(skill: &i32, level: &i32) -> String { format!("该项目需要技能 {skill} 达到 {level} 级。") }
/// Industry activity slots are occupied.
pub fn industry_slots_full_message(slots: i64) -> String { format!("你在这项活动上的全部 {slots} 个工作槽位都已占用。") }
/// Materials required in the facility hangar.
pub fn required_materials_message(q: &i64, t: &i32) -> String { format!("你需要在此处物品机库中准备 {q} 个类型 {t}。") }

// Insurance (services/insurance.rs)
/// Only own assembled ship can be insured.
pub const INSURANCE_REQUIRES_OWN_ASSEMBLED_SHIP: &str = "只能为你自己已组装的舰船投保。";
/// Insurance quote expired.
pub const INSURANCE_QUOTE_EXPIRED: &str = "保险报价已过期，请重新打开窗口。";

// Inventory (services/inventory.rs)
/// No permission to take from hangar division.
pub const NO_CORPORATION_HANGAR_TAKE_ROLE: &str = "你没有从该军团机库部门取出物品的职权。";
/// Not your corporation hangar.
pub const NOT_YOUR_CORPORATION_HANGAR: &str = "这不是你军团的机库。";
/// Locked container needs data or relic analyzer.
pub const CONTAINER_LOCKED_NEEDS_ANALYZER: &str = "这个容器被锁住了。用数据分析仪或遗迹分析仪破解它。";

// Mail (services/mail.rs)
/// At least one recipient required.
pub const MAIL_RECIPIENT_REQUIRED: &str = "请至少选择一名收件人。";
/// Too many recipients.
pub const TOO_MANY_MAIL_RECIPIENTS: &str = "收件人过多。";
/// Mail only to own corporation.
pub const MAIL_ONLY_TO_OWN_CORPORATION: &str = "你只能给自己的军团发送邮件。";
/// Label limit reached.
pub const LABEL_LIMIT_REACHED: &str = "标签数量已达上限。";

// Market (services/market.rs)
/// Invalid price.
pub const INVALID_PRICE: &str = "价格无效。";
/// No market at location.
pub const NO_MARKET_AT_LOCATION: &str = "该地点没有市场。";
/// Order is closed.
pub const MARKET_ORDER_CLOSED: &str = "该订单已关闭。";
/// Sell item from its station item hangar.
pub const MARKET_SELL_ITEM_MUST_BE_IN_STATION_HANGAR: &str = "只能在物品所在空间站出售你物品机库中的物品。";
/// Open market order limit.
pub fn market_order_limit_message(open: i64) -> String { format!("你已有 {open} 个未完成的市场订单，达到了交易技能允许的上限。") }

// Mining (services/mining.rs)
/// Target outside mining range.
pub const TARGET_OUTSIDE_MINING_RANGE: &str = "目标超出了采矿设备的范围。";
/// Mining hold full.
pub const MINING_HOLD_FULL: &str = "你的采矿舱已经满了。";
/// Cargo hold full.
pub const CARGO_HOLD_FULL: &str = "你的货柜舱已经满了。";
/// Reprocessed output exceeds stack capacity.
pub const REPROCESS_OUTPUT_STACK_LIMIT: &str = "精炼产物数量超过物品堆叠容量。";
/// Must dock to reprocess.
pub const MUST_DOCK_TO_REPROCESS: &str = "你必须停靠在空间站里才能进行精炼。";
/// Insufficient ISK for reprocessing tax.
pub const INSUFFICIENT_ISK_FOR_REPROCESSING_TAX: &str = "你的 ISK 余额不足以支付精炼税。";

// Missions (services/missions.rs)
/// Target cleared; complete and claim reward.
pub const MISSION_TARGET_CLEARED_COMPLETE: &str = "你已经清除了目标。完成任务，领取你的报酬吧。";
/// Ready; complete and claim reward.
pub const MISSION_READY_COMPLETE: &str = "一切就绪。完成任务，领取你的报酬吧。";
/// Find named person and deliver there.
pub const MISSION_FIND_AGENT_DELIVER: &str = "去找我说的那个人吧，完成后在那里交付。";
/// Bring cargo to station hangar, then return.
pub const MISSION_DELIVER_TO_STATION_HANGAR: &str = "把货物带到这个空间站的机库里，然后再来找我。";
/// Unfinished business remains.
pub const MISSION_INCOMPLETE: &str = "我们还有事情没办完。";
/// Target remains; report after completion.
pub const MISSION_TARGET_REMAINS: &str = "目标还在那里。完成后回来向我报告。";
/// Well done.
pub const MISSION_WELL_DONE: &str = "干得好。";
/// Waiting for pilot.
pub const MISSION_WAITING_FOR_PILOT: &str = "我一直在等你。";
/// No mission currently available.
pub const NO_MISSION_AVAILABLE: &str = "我目前没有可以交给你的任务。";
/// Greeting with available work.
pub const MISSION_GREETING_WITH_WORK: &str = "你好，飞行员。我这里有些工作需要人手。";
/// Nothing for pilot currently.
pub const NO_CURRENT_MISSION: &str = "现在我这里没有你的事。";
/// Mission accepted confirmation.
pub const MISSION_ACCEPTED_CONFIRMATION: &str = "就这么定了。";
/// Destination logged after acceptance.
pub const MISSION_ACCEPTED_DESTINATION_LOGGED: &str = "很好。目标位置已经加入你的日志，出发吧。";
/// Mission declined.
pub const MISSION_DECLINED: &str = "好吧，也许下次吧。";
/// Mission reward delivery.
pub const MISSION_REWARD_DELIVERY: &str = "干得漂亮！这是你的报酬。";
/// Mission quit message.
pub const MISSION_QUIT: &str = "真遗憾。也许下次你能做到。";
/// Pilot greeting.
pub const PILOT_GREETING: &str = "你好，飞行员。";
/// Agent standing requirement.
pub fn mission_standing_requirement_message(needed: f64, have: f64) -> String { format!("我只和信誉良好的飞行员合作。你需要对我、我的军团或势力拥有至少 {needed:.1} 的有效声望（目前为 {have:.2}）。") }

// Offices (services/offices.rs)
/// NPC corporations cannot rent offices.
pub const NPC_CORPORATION_CANNOT_RENT_OFFICE: &str = "NPC 军团不能租用办公室。";
/// No permission to rent office.
pub const NO_OFFICE_RENTAL_ROLE: &str = "你没有租用办公室的职权。";
/// Office rent changed; query again.
pub const OFFICE_RENT_CHANGED: &str = "办公室租金已变动，请重新查询。";
/// No permission to release office.
pub const NO_OFFICE_RELEASE_ROLE: &str = "你没有退租办公室的职权。";
/// Empty hangar before office release.
pub const EMPTY_CORPORATION_HANGAR_BEFORE_RELEASE: &str = "请先清空军团机库再退租办公室。";

// Organizations (services/orgs.rs)
/// Insufficient role for operation.
pub const INSUFFICIENT_ROLE_FOR_OPERATION: &str = "你没有执行此操作所需的职权。";

// Ship (services/ship.rs)
/// Must be in space.
pub const MUST_BE_IN_SPACE: &str = "你必须在太空中。";
/// Weapons timer prevents an action.
pub fn weapons_timer_message(secs: i64, what: &str) -> String { format!("武器计时器仍在生效，{secs} 秒内无法{what}。") }
/// Docking action in a weapons timer refusal.
pub const WEAPONS_TIMER_DOCK: &str = "停靠";
/// Jumping action in a weapons timer refusal.
pub const WEAPONS_TIMER_JUMP: &str = "跳跃";

// Standings (services/standings.rs)
/// Loyalty-point store requires docking.
pub const STATION_DOCK_REQUIRED_FOR_LP_STORE: &str = "你必须停靠在空间站内才能使用忠诚点商店。";
/// Loyalty-point offer is unavailable.
pub const LP_OFFER_UNAVAILABLE: &str = "该兑换项已不可用。";
/// Available loyalty point balance.
pub fn loyalty_balance_message(before: i64) -> String { format!("你在该军团只有 {before} 忠诚点。") }

// Station (services/station.rs)
/// Amplification matrix is unavailable.
pub const AMPLIFICATION_MATRIX_UNAVAILABLE: &str = "增幅矩阵暂不可用。";

// Store (services/store.rs)
/// Daily gift category.
pub const DAILY_GIFT_CATEGORY_NAME: &str = "每日礼包";
/// Ship skin category.
pub const SHIP_SKIN_CATEGORY_NAME: &str = "舰船涂装";
/// Skills and services category.
pub const SKILLS_AND_SERVICES_CATEGORY_NAME: &str = "技能与服务";
/// Apparel category; includes SQL fallback.
pub const APPAREL_CATEGORY_NAME: &str = "服饰";
/// Supply crate category.
pub const SUPPLY_CRATE_CATEGORY_NAME: &str = "补给箱";
/// Daily free subcategory.
pub const DAILY_FREE_CATEGORY_NAME: &str = "每日免费";
/// Skill injector subcategory.
pub const SKILL_INJECTOR_CATEGORY_NAME: &str = "技能注入";
/// Amarr category.
pub const AMARR_CATEGORY_NAME: &str = "艾玛";
/// Caldari category.
pub const CALDARI_CATEGORY_NAME: &str = "加达里";
/// Gallente category.
pub const GALLENTE_CATEGORY_NAME: &str = "盖伦特";
/// Minmatar category.
pub const MINMATAR_CATEGORY_NAME: &str = "米玛塔尔";
/// Other faction category.
pub const OTHER_FACTION_CATEGORY_NAME: &str = "其他";

// Client message-catalogue names
/// Client catalogue key from clientDogmaLocation.LinkWeapons.
pub const MSG_CANT_LINK_MODULE_NOT_ONLINE: &str = "CantLinkModuleNotOnline";
/// Client fittingDogmaLocation.py refusal for an activation-blocked module.
pub const MSG_MODULE_IS_BLOCKED: &str = "ModuleIsBlocked";
/// Client message `BkmMustBeInFlight`.
pub const MSG_BKM_MUST_BE_IN_FLIGHT: &str = "BkmMustBeInFlight";
/// Client message `BlueprintNotFound`.
pub const MSG_BLUEPRINT_NOT_FOUND: &str = "BlueprintNotFound";
/// Client message `BookmarkFolderNoLongerThere`.
pub const MSG_BOOKMARK_FOLDER_NO_LONGER_THERE: &str = "BookmarkFolderNoLongerThere";
/// Client message `CanNotPlaceActiveShipInAnotherHangar`.
pub const MSG_CAN_NOT_PLACE_ACTIVE_SHIP_IN_ANOTHER_HANGAR: &str = "CanNotPlaceActiveShipInAnotherHangar";
/// Client message `CantActivateReloadingModule`.
pub const MSG_CANT_ACTIVATE_RELOADING_MODULE: &str = "CantActivateReloadingModule";
/// Client message `CantApplyTwiceToAlliance`.
pub const MSG_CANT_APPLY_TWICE_TO_ALLIANCE: &str = "CantApplyTwiceToAlliance";
/// Client message `CantAssembleModularShipInSpace`.
pub const MSG_CANT_ASSEMBLE_MODULAR_SHIP_IN_SPACE: &str = "CantAssembleModularShipInSpace";
/// Client message `CantBoardShipNotInHangar`.
pub const MSG_CANT_BOARD_SHIP_NOT_IN_HANGAR: &str = "CantBoardShipNotInHangar";
/// Client message `CantBoardUnassembledShip`.
pub const MSG_CANT_BOARD_UNASSEMBLED_SHIP: &str = "CantBoardUnassembledShip";
/// Client message `CantRepackageActiveShip`.
pub const MSG_CANT_REPACKAGE_ACTIVE_SHIP: &str = "CantRepackageActiveShip";
/// Client message `CantUndockNotInStation`.
pub const MSG_CANT_UNDOCK_NOT_IN_STATION: &str = "CantUndockNotInStation";
/// Client message `CharacterNotFound`.
pub const MSG_CHARACTER_NOT_FOUND: &str = "CharacterNotFound";
/// Client message `CharacterNotReadyForDeletion`.
pub const MSG_CHARACTER_NOT_READY_FOR_DELETION: &str = "CharacterNotReadyForDeletion";
/// Client message `CharNameInvalid`.
pub const MSG_CHAR_NAME_INVALID: &str = "CharNameInvalid";
/// Client `eve/client/script/ui/services/skillsvc.py:1028-1029` combine validation.
pub const MSG_COMBINE_SKILL_INJECTOR_TOO_FEW_INJECTORS: &str = "CombineSkillInjectorTooFewInjectors";
/// Client `eve/client/script/ui/services/skillsvc.py:774-776` skillbook validation.
pub const MSG_ITEM_NOT_A_SKILL: &str = "ItemNotASkill";
/// Client characterskills/common/purchase.py: duplicate purchase request.
pub const MSG_SKILL_PURCHASE_UNKNOWN_ERROR: &str = "SkillPurchaseUnknownError";
/// Client characterskills/common/purchase.py: skill unavailable for purchase.
pub const MSG_SKILL_UNAVAILABLE_FOR_PURCHASE: &str = "SkillUnavailableForPurchase";
/// Client characterskills/common/purchase.py: skill already learned.
pub const MSG_CHARACTER_ALREADY_KNOWS_SKILL: &str = "CharacterAlreadyKnowsSkill";
/// Client message `ConContractNotFound`.
pub const MSG_CON_CONTRACT_NOT_FOUND: &str = "ConContractNotFound";
/// Client message `ConContractNotOutstanding`.
pub const MSG_CON_CONTRACT_NOT_OUTSTANDING: &str = "ConContractNotOutstanding";
/// Client message `ConExceedsMaxVolume`.
pub const MSG_CON_EXCEEDS_MAX_VOLUME: &str = "ConExceedsMaxVolume";
/// Client message `ConfirmRepackageSomethingWithUpgrades`.
pub const MSG_CONFIRM_REPACKAGE_SOMETHING_WITH_UPGRADES: &str = "ConfirmRepackageSomethingWithUpgrades";
/// Client message `CorpBulletinHasBeenEdited`.
pub const MSG_CORP_BULLETIN_HAS_BEEN_EDITED: &str = "CorpBulletinHasBeenEdited";
/// Client message `CorpBulletinNotFound`.
pub const MSG_CORP_BULLETIN_NOT_FOUND: &str = "CorpBulletinNotFound";
/// Client message `CorpTooManyBulletins`.
pub const MSG_CORP_TOO_MANY_BULLETINS: &str = "CorpTooManyBulletins";
/// Client message `CustomNotify`.
pub const MSG_CUSTOM_NOTIFY: &str = "CustomNotify";
/// `shipSkinApplicationSvc.py` catches this message to refresh the applied state.
pub const MSG_SKIN_ALREADY_APPLIED: &str = "SkinAlreadyApplied";
/// `shipskins/storage.py` raises this condition for an existing permanent license.
pub const MSG_SKIN_ALREADY_LICENSED: &str = "SkinAlreadyLicensed";
/// Client message `CannotApplyFreePointsDoNotHaveSkill`.
pub const MSG_CANNOT_APPLY_FREE_POINTS_DO_NOT_HAVE_SKILL: &str = "CannotApplyFreePointsDoNotHaveSkill";
/// Client message `InjectorSkillPointLimitReached`.
pub const MSG_INJECTOR_SKILL_POINT_LIMIT_REACHED: &str = "InjectorSkillPointLimitReached";
/// Client inventory refusal key used by `invItemFunctions.TrashInvItems`.
pub const MSG_CANNOT_TRASH_ITEM: &str = "CannotTrashItem";
/// Client kill-right refusal keys.
pub const MSG_NO_VALID_KILL_RIGHT: &str = "NoValidKillRight";
pub const MSG_KILL_RIGHT_EXPIRED: &str = "KillRightExpired";
pub const MSG_KILL_RIGHT_NOT_FOR_SALE: &str = "KillRightNotForSale";
/// Client message `CustomQuestion`.
pub const MSG_CUSTOM_QUESTION: &str = "CustomQuestion";
/// Client message `DeniedActivateInWarp`.
pub const MSG_DENIED_ACTIVATE_IN_WARP: &str = "DeniedActivateInWarp";
/// Client message `DeniedActivateTargetNotPresent`.
pub const MSG_DENIED_ACTIVATE_TARGET_NOT_PRESENT: &str = "DeniedActivateTargetNotPresent";
/// Client message `DockingRequestDenied`.
pub const MSG_DOCKING_REQUEST_DENIED: &str = "DockingRequestDenied";
/// Client message `FleetError`.
pub const MSG_FLEET_ERROR: &str = "FleetError";
/// Client message `IndustryBlueprintAccessDenied`.
pub const MSG_INDUSTRY_BLUEPRINT_ACCESS_DENIED: &str = "IndustryBlueprintAccessDenied";
/// Client message `IndustryJobAccessDenied`.
pub const MSG_INDUSTRY_JOB_ACCESS_DENIED: &str = "IndustryJobAccessDenied";
/// Client message `IndustryJobNotFound`.
pub const MSG_INDUSTRY_JOB_NOT_FOUND: &str = "IndustryJobNotFound";
/// Client message `InsCouldNotFindItem`.
pub const MSG_INS_COULD_NOT_FIND_ITEM: &str = "InsCouldNotFindItem";
/// Client message `InsureShipFailedSingleContract`.
pub const MSG_INSURE_SHIP_FAILED_SINGLE_CONTRACT: &str = "InsureShipFailedSingleContract";
/// Client message `JumpGateHasNoDestination`.
pub const MSG_JUMP_GATE_HAS_NO_DESTINATION: &str = "JumpGateHasNoDestination";
/// Client message `MailLabelMustProvideName`.
pub const MSG_MAIL_LABEL_MUST_PROVIDE_NAME: &str = "MailLabelMustProvideName";
/// Client message `MktOrderDidNotMatch`.
pub const MSG_MKT_ORDER_DID_NOT_MATCH: &str = "MktOrderDidNotMatch";
/// Client message `NoCharges`.
pub const MSG_NO_CHARGES: &str = "NoCharges";
/// Client message `NotAtStation`.
pub const MSG_NOT_AT_STATION: &str = "NotAtStation";
/// Client message `NotCloseEnoughToJump`.
pub const MSG_NOT_CLOSE_ENOUGH_TO_JUMP: &str = "NotCloseEnoughToJump";
/// Client message `NotEnoughMoney`.
pub const MSG_NOT_ENOUGH_MONEY: &str = "NotEnoughMoney";
/// Client message `OnlyCEOCanResign`.
pub const MSG_ONLY_CEO_CAN_RESIGN: &str = "OnlyCEOCanResign";
/// Client `eve/client/script/ui/shared/neocom/attributes.py:241` validation.
pub const MSG_RESPEC_ATTRIBUTES_TOO_LOW: &str = "RespecAttributesTooLow";
/// Client `eve/client/script/ui/shared/neocom/attributes.py:243` validation.
pub const MSG_RESPEC_ATTRIBUTES_TOO_HIGH: &str = "RespecAttributesTooHigh";
/// Client `eve/client/script/ui/shared/neocom/attributes.py:250` validation.
pub const MSG_RESPEC_ATTRIBUTES_MISALLOCATED: &str = "RespecAttributesMisallocated";
/// Client `eve/client/script/ui/shared/neocom/attributes.py:267` entitlement refusal.
pub const MSG_RESPEC_TOO_SOON: &str = "RespecTooSoon";
/// Client skillQueueSvc.py queue validation refusals.
pub const MSG_QUEUE_INVALID_POSITION: &str = "QueueInvalidPosition";
pub const MSG_QUEUE_SKILL_NOT_UPLOADED: &str = "QueueSkillNotUploaded";
pub const MSG_QUEUE_PREVIOUSLY_TRAINED: &str = "QueueCannotTrainPreviouslyTrainedSkills";
pub const MSG_QUEUE_PAST_MAXIMUM: &str = "QueueCannotTrainPastMaximumLevel";
pub const MSG_QUEUE_LEVELS_OUT_OF_ORDER: &str = "QueueCannotPlaceSkillLevelsOutOfOrder";
pub const MSG_QUEUE_BEFORE_REQUIREMENTS: &str = "QueueCannotPlaceSkillBeforeRequirements";
pub const MSG_QUEUE_AFTER_DEPENDENTS: &str = "QueueCannotPlaceSkillAfterDependentSkills";
pub const MSG_QUEUE_ALREADY_PRESENT: &str = "QueueSkillAlreadyPresent";
pub const MSG_QUEUE_TOO_MANY: &str = "QueueTooManySkills";
/// Client message `RepackageBeforeSelling`.
pub const MSG_REPACKAGE_BEFORE_SELLING: &str = "RepackageBeforeSelling";
/// Client `eve/client/script/ui/services/skillsvc.py:1004-1005` extraction validation.
pub const MSG_SKILL_EXTRACTION_NOT_ENOUGH_SP: &str = "SkillExtractionNotEnoughSP";
/// Client `eve/client/script/ui/services/skillsvc.py:1006-1009` extraction token validation.
pub const MSG_TOKEN_REQUIRED_FOR_SKILL_EXTRACTION: &str = "TokenRequiredForSkillExtraction";
/// Client `eve/client/script/ui/services/skillsvc.py:1000-1001` docking validation.
pub const MSG_SKILL_EXTRACTOR_NOT_DOCKED_IN_STATION: &str = "SkillExtractorNotDockedInStation";
/// Client message `ShipNotFound`.
pub const MSG_SHIP_NOT_FOUND: &str = "ShipNotFound";
/// Client message `SlashError`.
pub const MSG_SLASH_ERROR: &str = "SlashError";
/// Client message `TargetingAttemptCancelled`.
pub const MSG_TARGETING_ATTEMPT_CANCELLED: &str = "TargetingAttemptCancelled";
