//! Server-authored player-visible text and client message-catalogue keys.
//! The Infinity client displays Chinese text in Simplified Chinese; message
//! names are keys in its catalogue, including `eveexceptions.UserError` and
//! the client's slash service. This module reads no database and does not
//! implement localisation, data-derived names, or protocol identifiers.

pub(crate) mod bounty;
pub(crate) mod combat;
pub(crate) mod contacts;
pub(crate) mod effects;
pub(crate) mod fitting;
pub(crate) mod gm;
pub(crate) mod repair;
pub(crate) mod scheduler;
pub(crate) mod lua;
pub(crate) mod loyalty;
pub(crate) mod standings;
pub(crate) mod trade;

// Client fitting and online UserError catalogue keys.
pub(crate) const MSG_FITTING_HAS_SKILL_PREREQUISITES: &str = "FittingHasSkillPrerequisites";
pub(crate) const MSG_ONLINE_HAS_SKILL_PREREQUISITES: &str = "OnlineHasSkillPrerequisites";
pub(crate) const MSG_NOT_ENOUGH_CPU: &str = "NotEnoughCpu";
pub(crate) const MSG_NOT_ENOUGH_POWER: &str = "NotEnoughPower";
pub(crate) const MSG_CANT_FIT_TOO_MANY_BY_GROUP: &str = "CantFitTooManyByGroup";
pub(crate) const MSG_CANT_FIT_TOO_MANY_BY_TYPE: &str = "CantFitTooManyByType";
pub(crate) const MSG_CANNOT_ONLINE_REACHED_MAX_GROUP_ONLINE: &str = "CannotOnlineReachedMaxGroupOnline";
pub(crate) const MSG_EFFECT_CROWDED_OUT: &str = "EffectCrowdedOut";
pub(crate) const MSG_MODULE_NOT_POWERED: &str = "ModuleNotPowered";
pub(crate) const MSG_NO_SUITABLE_SLOTS_FOR_THIS_SUBSYSTEM: &str = "NoSuitableSlotsForThisSubSystem";
pub(crate) const MSG_CANNOT_FIT_SUBSYSTEM_NOT_SHIP: &str = "CannotFitSubSystemNotShip";

/// Proxy usurp reason from `carbon/common/script/net/machoNet.py`.
pub(crate) const SESSION_USURPED: &str = "The user's connection has been usurped on the proxy";
/// Generic connection refusal mapped by `carbon/common/script/net/ExceptionMappingGPCS.py:16`.
pub(crate) const ACL_NOTACCEPTING: &str = "ACL_NOTACCEPTING";

// Alliances (services/alliances.rs)
/// Alliance action requires a player-corporation CEO.
pub(crate) const PLAYER_CORP_CEO_REQUIRED: &str = "只有玩家军团的 CEO 才能这样做。";
/// Joining an alliance when the corporation already has one.
pub(crate) const CORP_ALREADY_IN_ALLIANCE: &str = "你的军团已经在一个联盟中。";
/// Alliance creation name length validation.
pub(crate) const ALLIANCE_NAME_LENGTH_INVALID: &str = "联盟名称长度必须为 4 到 50 个字符。";
/// Alliance short-name validation.
pub(crate) const ALLIANCE_SHORT_NAME_INVALID: &str = "联盟简称必须为 2 到 5 个字母、数字、点或短横线。";
/// Alliance uniqueness validation.
pub(crate) const ALLIANCE_NAME_OR_SHORT_NAME_TAKEN: &str = "该联盟名称或简称已被占用。";
/// Alliance join target corporation missing.
pub(crate) const CORP_NOT_FOUND: &str = "该军团已不存在。";
/// The target corporation already belongs to an alliance.
pub(crate) const TARGET_CORP_ALREADY_IN_ALLIANCE: &str = "该军团已经在一个联盟中。";
/// Choosing alliance executor.
pub(crate) const CORP_CEO_REQUIRED_TO_CHOOSE_EXECUTOR: &str = "只有军团 CEO 才能选择支持的执行军团。";
/// Executor/removal target is not an alliance member.
pub(crate) const CORP_NOT_ALLIANCE_MEMBER: &str = "该军团不是联盟成员。";
/// Alliance lookup/application operation.
pub(crate) const ALLIANCE_NOT_FOUND: &str = "该联盟已不存在。";
/// Withdrawing alliance application.
pub(crate) const CORP_CEO_REQUIRED_TO_WITHDRAW_ALLIANCE_APPLICATION: &str = "只有军团 CEO 才能撤回联盟申请。";
/// Editing alliance information.
pub(crate) const EXECUTOR_CORP_DIRECTOR_REQUIRED_TO_EDIT_ALLIANCE: &str = "只有执行军团的董事才能修改联盟信息。";
/// Handling alliance application.
pub(crate) const EXECUTOR_CORP_DIRECTOR_REQUIRED_TO_HANDLE_APPLICATION: &str = "只有执行军团的董事才能处理联盟申请。";
/// Alliance application missing.
pub(crate) const ALLIANCE_APPLICATION_NOT_FOUND: &str = "该申请已不存在。";
/// Alliance application status validation.
pub(crate) const ALLIANCE_APPLICATION_STATUS_INVALID: &str = "无效的申请状态。";
/// Alliance member removal permission.
pub(crate) const NOT_AUTHORIZED_TO_REMOVE_CORP_FROM_ALLIANCE: &str = "你无法将该军团移出联盟。";
/// Alliance relationship update permission.
pub(crate) const EXECUTOR_CORP_DIRECTOR_REQUIRED_TO_SET_RELATIONSHIP: &str = "只有执行军团的董事才能设置联盟关系。";
/// War declaration unavailable.
pub(crate) const WAR_DECLARATION_NOT_AVAILABLE: &str = "战争尚未开放。";
/// Alliance bill payment permission.
pub(crate) const BILL_PAYMENT_NOT_AUTHORIZED: &str = "你无法支付该账单。";

// Bills (services/bills.rs)
/// Bill is paid or missing.
pub(crate) const BILL_ALREADY_PAID_OR_MISSING: &str = "该账单已支付或不存在。";
/// Corporate bill payment permission.
pub(crate) const BILL_PAYMENT_PERMISSION_REQUIRED: &str = "你没有支付该账单的职权。";
/// Personal bill ownership check.
pub(crate) const BILL_NOT_OWNED_BY_CHARACTER: &str = "这不是你的账单。";
/// Bill autopay settings permission.
pub(crate) const ACCOUNTANT_REQUIRED_FOR_AUTOPAY_SETTINGS: &str = "只有会计才能修改自动付款设置。";

// Bookmarks (services/bookmarks.rs)
/// Bookmark target unavailable.
pub(crate) const BOOKMARK_TARGET_MOVED_OR_MISSING: &str = "目标已不在原处。";
/// Bookmarking requires a fully scanned signature.
pub(crate) const BOOKMARK_REQUIRES_FULL_SCAN: &str = "请先将信号扫描到 100% 再保存位置。";
/// Shared bookmark folders are unavailable.
pub(crate) const SHARED_BOOKMARK_FOLDERS_UNSUPPORTED: &str = "暂不支持共享书签文件夹。";

// Calendar (services/calendar.rs)
/// Calendar event missing.
pub(crate) const CALENDAR_EVENT_NOT_FOUND: &str = "该活动已不存在。";
/// Calendar event edit permission.
pub(crate) const CALENDAR_EVENT_EDIT_NOT_AUTHORIZED: &str = "你无法修改该活动。";
/// Corporation calendar event creation permission.
pub(crate) const PLAYER_CORP_DIRECTOR_REQUIRED_FOR_CORP_EVENT: &str = "只有玩家军团的董事才能创建军团活动。";
/// Alliance calendar events unavailable.
pub(crate) const ALLIANCE_EVENTS_UNSUPPORTED: &str = "暂不支持联盟活动。";

// Character selection (services/charsel.rs)
/// A generated character name.
pub(crate) fn random_character_name(first: &str, last: &str, suffix: usize) -> String { format!("{} {} {}", first, last, suffix) }
/// Generated character first name.
pub(crate) const RANDOM_FIRST_AREN: &str = "Aren";
/// Generated character first name.
pub(crate) const RANDOM_FIRST_KAEL: &str = "Kael";
/// Generated character first name.
pub(crate) const RANDOM_FIRST_SORA: &str = "Sora";
/// Generated character first name.
pub(crate) const RANDOM_FIRST_VEYRA: &str = "Veyra";
/// Generated character first name.
pub(crate) const RANDOM_FIRST_TOMA: &str = "Toma";
/// Generated character first name.
pub(crate) const RANDOM_FIRST_ILSA: &str = "Ilsa";
/// Generated character first name.
pub(crate) const RANDOM_FIRST_DARO: &str = "Daro";
/// Generated character first name.
pub(crate) const RANDOM_FIRST_MIRA: &str = "Mira";
/// Generated character first name.
pub(crate) const RANDOM_FIRST_LUKAS: &str = "Lukas";
/// Generated character first name.
pub(crate) const RANDOM_FIRST_NADIA: &str = "Nadia";
/// Generated character first name.
pub(crate) const RANDOM_FIRST_JOREN: &str = "Joren";
/// Generated character first name.
pub(crate) const RANDOM_FIRST_TESSA: &str = "Tessa";
/// Generated character surname.
pub(crate) const RANDOM_LAST_VALE: &str = "Vale";
/// Generated character surname.
pub(crate) const RANDOM_LAST_OKARO: &str = "Okaro";
/// Generated character surname.
pub(crate) const RANDOM_LAST_THORNE: &str = "Thorne";
/// Generated character surname.
pub(crate) const RANDOM_LAST_KASSEN: &str = "Kassen";
/// Generated character surname.
pub(crate) const RANDOM_LAST_RUUN: &str = "Ruun";
/// Generated character surname.
pub(crate) const RANDOM_LAST_HALVIK: &str = "Halvik";
/// Generated character surname.
pub(crate) const RANDOM_LAST_MOREAU: &str = "Moreau";
/// Generated character surname.
pub(crate) const RANDOM_LAST_SANDOR: &str = "Sandor";
/// Generated character surname.
pub(crate) const RANDOM_LAST_IKONEN: &str = "Ikonen";
/// Generated character surname.
pub(crate) const RANDOM_LAST_DRAKE: &str = "Drake";
/// Generated character surname.
pub(crate) const RANDOM_LAST_ARVEL: &str = "Arvel";
/// Generated character surname.
pub(crate) const RANDOM_LAST_CASTOR: &str = "Castor";

// Clones (services/clones.rs)
/// Jump-clone installation requires docking.
pub(crate) const STATION_DOCK_REQUIRED_FOR_JUMP_CLONE_INSTALL: &str = "你必须停靠在空间站内才能安装远距克隆。";
/// Duplicate jump clone at station.
pub(crate) const JUMP_CLONE_ALREADY_AT_STATION: &str = "你在这个空间站已经有一个远距克隆。";
/// Jump clone destination missing.
pub(crate) const JUMP_CLONE_NOT_FOUND: &str = "该远距克隆已不在原处。";
/// Jump destination is current station.
pub(crate) const ALREADY_AT_JUMP_CLONE_DESTINATION: &str = "你已经在该空间站中。";
/// Structure clone installation unavailable.
pub(crate) const CLONE_STRUCTURE_INSTALL_UNSUPPORTED: &str = "暂不支持建筑。";
/// Maximum installed jump clones.
pub(crate) fn jump_clone_limit(max: i64) -> String { format!("你的意识形态心理学技能允许 {max} 个远距克隆。") }
/// Remaining jump clone cooldown.
pub(crate) fn jump_clone_cooldown(hours: f64) -> String { format!("你需要再等 {hours:.1} 小时才能再次进行远距克隆跳跃。") }

// Combat (services/combat.rs)
/// Charge quantity unavailable.
pub(crate) const INSUFFICIENT_AMMUNITION: &str = "弹药数量不足。";
/// Mining module target validation.
pub(crate) const MINING_MODULE_TARGET_INVALID: &str = "只能对小行星、冰矿或气云使用采矿设备。";
/// Target distance exceeds lock range.
pub(crate) fn target_lock_range(gap: f64, range: f64) -> String { format!("目标距离 {:.1} 公里，超出了你的锁定距离（{:.1} 公里）。", gap, range) }
/// Maximum simultaneous targets.
pub(crate) fn target_limit(max_targets: usize) -> String { format!("你最多只能同时锁定 {} 个目标。", max_targets) }
/// Target distance exceeds mining equipment range.
pub(crate) fn mining_range(range: f64) -> String { format!("目标超出了采矿设备的范围（{:.1} 公里）。", range) }

// Contracts (services/contracts.rs)
/// Corporation contracts unavailable.
pub(crate) const CORPORATION_CONTRACTS_UNSUPPORTED: &str = "暂不支持军团合同。";
/// Contract type required.
pub(crate) const CONTRACT_TYPE_REQUIRED: &str = "请选择合同类型。";
/// Contract monetary values must not be negative.
pub(crate) const CONTRACT_AMOUNT_NEGATIVE: &str = "金额不能为负数。";
/// Contract cannot be assigned to issuer.
pub(crate) const CONTRACT_SELF_ASSIGNMENT_FORBIDDEN: &str = "你不能将合同指定给自己。";
/// Auction requires offered items.
pub(crate) const AUCTION_REQUIRES_ITEMS: &str = "拍卖需要有待售物品。";
/// Auction buyout must not undercut start price.
pub(crate) const BUYOUT_BELOW_START_PRICE: &str = "一口价不能低于起拍价。";
/// Courier contract requires items.
pub(crate) const COURIER_REQUIRES_ITEMS: &str = "运输合同需要有待运送的物品。";
/// Item exchange needs offer or request.
pub(crate) const ITEM_EXCHANGE_REQUIRES_OFFER_OR_REQUEST: &str = "物品交换需要提供或索取物品。";
/// Contract item limit exceeded.
pub(crate) const CONTRACT_ITEM_LIMIT_EXCEEDED: &str = "一份合同中的物品过多。";
/// Contract item missing.
pub(crate) const CONTRACT_ITEM_MISSING: &str = "其中一件物品已不在原处。";
/// Contract items must be in start-station hangar.
pub(crate) const CONTRACT_ITEM_MUST_BE_IN_START_HANGAR: &str = "只有起始空间站物品机库中的物品才能放入合同。";
/// Courier package cannot be traded.
pub(crate) const COURIER_PACKAGE_NOT_TRADEABLE: &str = "运输包裹不能交易。";
/// Contract item quantity insufficient.
pub(crate) const CONTRACT_ITEM_QUANTITY_INSUFFICIENT: &str = "其中一件物品的数量不足。";
/// Issuer cannot accept own contract.
pub(crate) const CONTRACT_SELF_ACCEPTANCE_FORBIDDEN: &str = "你不能接受自己的合同。";
/// Contract is assigned to another party.
pub(crate) const CONTRACT_ASSIGNED_TO_OTHER: &str = "该合同已指定给其他人。";
/// Required contract items missing at station.
pub(crate) const CONTRACT_REQUIRED_ITEMS_MISSING: &str = "你在合同所在空间站的物品机库中没有全部所需物品。";
/// Only auctions accept bids.
pub(crate) const AUCTION_ONLY_BIDDABLE: &str = "拍卖只能出价竞拍。";
/// Courier delivery requires carrier.
pub(crate) const COURIER_DELIVERY_REQUIRES_ACCEPTOR: &str = "只有承运人才能交付这份合同。";
/// Courier package must be in destination hangar.
pub(crate) const COURIER_PACKAGE_MUST_BE_AT_DESTINATION: &str = "包裹必须位于目的地空间站的机库中。";
/// Courier cannot currently be failed.
pub(crate) const COURIER_CANNOT_BE_FAILED_NOW: &str = "现在无法将这份合同判定为失败。";
/// Only assignee may reject contract.
pub(crate) const CONTRACT_ASSIGNEE_REQUIRED_TO_REJECT: &str = "只有合同的指定对象才能拒绝合同。";
/// Contract status transition invalid.
pub(crate) const CONTRACT_STATUS_TRANSITION_INVALID: &str = "合同不能被设置为该状态。";
/// Only issuer may delete unfinished contract.
pub(crate) const CONTRACT_ISSUER_REQUIRED_TO_DELETE: &str = "只有发布者才能删除未完成的合同。";
/// Auction with bids cannot be deleted.
pub(crate) const BIDDED_AUCTION_CANNOT_BE_DELETED: &str = "已有出价的拍卖不能删除。";
/// Issuer cannot bid own auction.
pub(crate) const AUCTION_SELF_BID_FORBIDDEN: &str = "你不能对自己的拍卖出价。";
/// Auction assigned to another party.
pub(crate) const AUCTION_ASSIGNED_TO_OTHER: &str = "该拍卖已指定给其他人。";
/// Bid must exceed current and starting bids.
pub(crate) const BID_MUST_EXCEED_CURRENT_AND_START: &str = "你的出价必须高于当前出价和起拍价。";
/// Item no longer at original location.
pub(crate) const ITEM_NOT_AT_ORIGINAL_LOCATION: &str = "该物品已不在原处。";
/// Stack cannot be split this way.
pub(crate) const STACK_CANNOT_BE_SPLIT: &str = "无法这样拆分该堆叠。";
/// No contract creation request was supplied.
pub(crate) const CONTRACT_CREATION_REQUEST_MISSING: &str = "没有要创建的合同。";
/// Default name of a courier contract package.
pub(crate) fn contract_package_name(id: i64) -> String { format!("contract {id}") }
/// Outstanding contract limit.
pub(crate) fn contract_limit(open: i64, cap: i64) -> String { format!("你有 {open} 份未完成的合同，你的合同技能最多允许 {cap} 份。") }

// Corporations (services/corps.rs)
/// Corporation creation requires docking.
pub(crate) const STATION_DOCK_REQUIRED_TO_CREATE_CORP: &str = "你必须停靠在空间站内才能创建军团。";
/// Corporation name length validation.
pub(crate) const CORP_NAME_LENGTH_INVALID: &str = "军团名称长度必须为 4 到 50 个字符。";
/// Corporation short-name validation.
pub(crate) const CORP_SHORT_NAME_INVALID: &str = "军团代号必须为 2 到 5 个字母、数字、点或短横线。";
/// Corporation creation skill requirement.
pub(crate) const CORPORATION_MANAGEMENT_SKILL_REQUIRED: &str = "创建军团需要军团管理技能。";
/// CEO must resign or close current corporation first.
pub(crate) const ALREADY_CORP_CEO_MUST_RESIGN_OR_CLOSE: &str = "你已是 CEO，请先辞职或关闭你的军团。";
/// Corporation name or ticker is already taken.
pub(crate) const CORP_NAME_OR_SHORT_NAME_TAKEN: &str = "该军团名称或代号已被占用。";
/// NPC corporation cannot be left this way.
pub(crate) const CANNOT_LEAVE_NPC_CORP: &str = "不能用这种方式退出 NPC 军团。";
/// CEO cannot leave while other members remain.
pub(crate) const CORP_CEO_CANNOT_LEAVE_WITH_MEMBERS: &str = "军团还有其他成员时，CEO 不能退出。";
/// Corporation application already processed.
pub(crate) const CORP_APPLICATION_ALREADY_PROCESSED: &str = "该申请已处理完毕。";
/// Application does not belong to this character.
pub(crate) const CORP_APPLICATION_NOT_OWNED: &str = "这不是你的申请。";
/// Personnel manager permission required to handle applications.
pub(crate) const PERSONNEL_MANAGER_REQUIRED_FOR_APPLICATION: &str = "只有军团的人事经理才能处理申请。";
/// CEO must leave own corporation before joining another.
pub(crate) const CEO_MUST_LEAVE_CORP_BEFORE_JOINING: &str = "CEO 必须先离开自己的军团才能加入其他军团。";
/// Member role modification is unauthorized.
pub(crate) const CANNOT_MODIFY_MEMBER_ROLES: &str = "你无法修改该成员的职权。";
/// Director permission required for bulk member edits.
pub(crate) const DIRECTOR_REQUIRED_FOR_BULK_MEMBER_EDIT: &str = "只有董事才能批量修改成员。";
/// Director permission required to edit titles.
pub(crate) const DIRECTOR_REQUIRED_TO_EDIT_TITLES: &str = "只有董事才能修改头衔。";
/// Director permission required to delete titles.
pub(crate) const DIRECTOR_REQUIRED_TO_DELETE_TITLES: &str = "只有董事才能删除头衔。";
/// Member is ineligible to become CEO.
pub(crate) const MEMBER_CANNOT_BECOME_CEO: &str = "该成员无法成为 CEO。";
/// Member removal is unauthorized.
pub(crate) const CANNOT_REMOVE_MEMBER: &str = "你无法移除该成员。";
/// NPC corporation does not accept applications.
pub(crate) const NPC_CORP_DOES_NOT_ACCEPT_APPLICATIONS: &str = "NPC 军团不接受申请。";
/// Corporation is not currently accepting applications.
pub(crate) const CORP_NOT_RECRUITING: &str = "该军团目前不接受申请。";
/// Personnel manager permission required to invite.
pub(crate) const PERSONNEL_MANAGER_REQUIRED_TO_INVITE: &str = "只有人事经理才能发出邀请。";
/// Director permission required to edit division names.
pub(crate) const DIRECTOR_REQUIRED_TO_EDIT_DIVISION_NAMES: &str = "只有董事才能修改部门名称。";
/// Corporation shares are not available.
pub(crate) const CORP_SHARES_UNAVAILABLE: &str = "军团股份尚未开放。";
/// Director permission required to edit corporation information.
pub(crate) const DIRECTOR_REQUIRED_TO_EDIT_CORP_INFO: &str = "只有董事才能修改军团信息。";

// Corporation wallet (services/corpwallet.rs)
/// Transfer recipient must be player character or corporation.
pub(crate) const TRANSFER_RECIPIENT_NOT_PLAYER: &str = "只能向玩家角色或玩家军团转账。";
/// Wallet transfer validation failed.
pub(crate) const TRANSFER_INVALID: &str = "无效的转账。";
/// Department wallet withdrawal permission.
pub(crate) const DEPARTMENT_WALLET_WITHDRAW_UNAUTHORIZED: &str = "你没有从该部门钱包取款的职权。";

// Crimewatch (services/crimewatch.rs)
/// Criminal ship-destruction notification.
pub(crate) const CONCORD_DESTROYED_SHIP_FOR_CRIME: &str = "统合部因你的犯罪行为摧毁了你的舰船。";

// Exploration (services/exploration.rs)
/// Probe launcher has insufficient probes.
pub(crate) const INSUFFICIENT_PROBE_LAUNCHER_PROBES: &str = "探针发射器中的探针数量不足。";
/// Scan result no longer exists.
pub(crate) const SCAN_RESULT_GONE: &str = "扫描结果已经消失。";
/// Warp requires fully scanned signature.
pub(crate) const SIGNATURE_REQUIRES_FULL_SCAN_FOR_WARP: &str = "你需要先将该信号扫描到 100% 才能跃迁过去。";
/// Wormhole collapsed.
pub(crate) const WORMHOLE_COLLAPSED: &str = "这个虫洞已经坍塌了。";
/// Ship is polarized by wormhole.
pub(crate) const WORMHOLE_POLARIZED_FOR_SHIP: &str = "这个虫洞对你的舰船处于极化状态，暂时无法再次跃迁。";
/// A ship exceeds the wormhole mass limit.
pub(crate) fn wormhole_mass_limit(mass: f64) -> String { format!("你的舰船太重了，无法通过这个虫洞（最大 {:.0} 吨）。", mass) }
/// GM command needs a pilot in space.
pub(crate) const GM_UNDOCK_FIRST: &str = "undock first";
/// GM wormhole lookup found none.
pub(crate) const GM_NO_WORMHOLES: &str = "no wormholes anywhere yet";
/// GM wormhole lookup found a recently closed wormhole.
pub(crate) const GM_WORMHOLE_CLOSED: &str = "that wormhole just closed";
/// GM wormhole lookup found a non-wormhole site.
pub(crate) const GM_NOT_WORMHOLE: &str = "not a wormhole";
/// GM wormhole teleport result.
pub(crate) fn gm_wormhole_result(target: &str, type_id: i32, system: i64, dest_system: i64, dest_class: i64) -> String { format!("wormhole {} (type {}) in {} -> system {} (class {})", target, type_id, system, dest_system, dest_class) }
/// GM scan-all result.
pub(crate) fn gm_sites_scanned(count: usize) -> String { format!("{} sites scanned", count) }
/// Wormhole description in a GM site list.
pub(crate) fn gm_wormhole_site(type_id: i32, dest_system: i64, dest_class: i64) -> String { format!("wormhole {} -> {} (class {})", type_id, dest_system, dest_class) }
/// Data site label in a GM site list.
pub(crate) const GM_SITE_DATA: &str = "data";
/// Relic site label in a GM site list.
pub(crate) const GM_SITE_RELIC: &str = "relic";
/// Combat site label in a GM site list.
pub(crate) const GM_SITE_COMBAT: &str = "combat";
/// Anomaly label in a GM site list.
pub(crate) const GM_SITE_ANOMALY: &str = "anomaly";
/// Signature label in a GM site list.
pub(crate) const GM_SITE_SIGNATURE: &str = "signature";

// Fitting (services/fitting.rs)
/// Insufficient item quantity.
pub(crate) const INSUFFICIENT_ITEM_QUANTITY: &str = "该物品的数量不足。";

// Fleets (services/fleets.rs)
/// Initial fleet wing display name.
pub(crate) const FLEET_INITIAL_WING_NAME: &str = "联队 1";
/// Initial fleet squad display name.
pub(crate) const FLEET_INITIAL_SQUAD_NAME: &str = "中队 1";
/// Current character already belongs to a fleet.
pub(crate) const PILOT_ALREADY_IN_FLEET_SELF: &str = "你已在舰队中。";
/// Target pilot is not in the fleet.
pub(crate) const PILOT_NOT_IN_FLEET_TARGET: &str = "该飞行员不在舰队中。";
/// Fleet lookup/operation missing.
pub(crate) const FLEET_NOT_FOUND: &str = "该舰队已不存在。";
/// Fleet MOTD permission.
pub(crate) const FLEET_LEADER_REQUIRED_FOR_MOTD: &str = "只有舰队指挥官才能设置每日消息。";
/// Invite target already belongs to a fleet.
pub(crate) const PILOT_ALREADY_IN_FLEET: &str = "该飞行员已在舰队中。";
/// Pilot is not in fleet.
pub(crate) const PILOT_NOT_IN_FLEET: &str = "你不在该舰队中。";
/// Fleet member capacity reached.
pub(crate) const FLEET_FULL: &str = "舰队已满。";
/// Fleet invite dialog header.
pub(crate) const FLEET_INVITATION_HEADER: &str = "舰队邀请";
/// Fleet invitation expired.
pub(crate) const FLEET_INVITATION_EXPIRED: &str = "该邀请已过期。";
/// Fleet member removal permission.
pub(crate) const FLEET_LEADER_REQUIRED_TO_REMOVE_MEMBER: &str = "只有舰队指挥官才能移除成员。";
/// Fleet disband permission.
pub(crate) const FLEET_LEADER_REQUIRED_TO_DISBAND: &str = "只有舰队指挥官才能解散舰队。";
/// Fleet member movement permission.
pub(crate) const FLEET_LEADER_REQUIRED_TO_MOVE_MEMBER: &str = "只有舰队指挥官才能调动成员。";
/// Fleet wing creation permission.
pub(crate) const FLEET_LEADER_REQUIRED_TO_ADD_WING: &str = "只有舰队指挥官才能添加联队。";
/// Fleet wing lookup missing.
pub(crate) const FLEET_WING_NOT_FOUND: &str = "该联队已不存在。";
/// Fleet squad creation permission.
pub(crate) const FLEET_LEADER_REQUIRED_TO_ADD_SQUAD: &str = "只有舰队指挥官才能添加中队。";
/// Fleet structure cannot be removed while occupied.
pub(crate) const FLEET_STRUCTURE_MUST_BE_EMPTY: &str = "请先将其中的成员移出。";
/// Fleet structure permission.
pub(crate) const FLEET_LEADER_REQUIRED_TO_CHANGE_STRUCTURE: &str = "只有舰队指挥官才能调整舰队结构。";
/// Fleet hierarchy rename permission.
pub(crate) const FLEET_LEADER_REQUIRED_TO_RENAME_WING_OR_SQUAD: &str = "只有舰队指挥官才能重命名联队和中队。";
/// Fleet invitation question.
pub(crate) fn fleet_invitation(name: &str) -> String { format!("{name}邀请你加入舰队。是否接受？") }
/// Default fleet wing name.
pub(crate) fn fleet_wing_name(n: usize) -> String { format!("联队 {n}") }
/// Default fleet squad name.
pub(crate) fn fleet_squad_name(n: usize) -> String { format!("中队 {n}") }

// Hacking (services/hacking.rs)
/// Analyzer target validation.
pub(crate) const ANALYZER_TARGET_MUST_BE_DATA_OR_RELIC: &str = "分析仪只能用于数据和遗迹地点的容器。";
/// Container already hacked.
pub(crate) const HACK_CONTAINER_ALREADY_HACKED: &str = "这个容器已经被破解了。";
/// Relic analyzer requirement.
pub(crate) const RELIC_CONTAINER_REQUIRES_RELIC_ANALYZER: &str = "这是遗迹容器，需要使用遗迹分析仪。";
/// Data analyzer requirement.
pub(crate) const DATA_CONTAINER_REQUIRES_DATA_ANALYZER: &str = "这是数据容器，需要使用数据分析仪。";
/// Analyzer target is out of range.
pub(crate) fn analyzer_range(gap: f64, range: f64) -> String { format!("目标太远了（{:.1} 公里），分析仪的范围是 {:.1} 公里。", gap, range) }

// Industry (services/industry.rs)
/// Facility lacks industry services.
pub(crate) const NO_INDUSTRIAL_SERVICES: &str = "该空间站没有工业服务。";
/// Project exceeds duration limit.
pub(crate) const INDUSTRY_PROJECT_TOO_LONG: &str = "该项目耗时过长，请减少流程数。";
/// Corporation industry unsupported.
pub(crate) const CORPORATION_INDUSTRY_UNSUPPORTED: &str = "暂不支持军团工业项目。";
/// Blueprint is already in use.
pub(crate) const BLUEPRINT_IN_USE: &str = "该蓝图正在被一个项目使用。";
/// Activity unavailable at facility.
pub(crate) const INDUSTRY_ACTIVITY_UNAVAILABLE: &str = "该空间站不提供这项活动。";
/// Blueprint must be at facility.
pub(crate) const BLUEPRINT_MUST_BE_AT_FACILITY: &str = "蓝图必须位于该设施中。";
/// Blueprint cannot be used for activity.
pub(crate) const BLUEPRINT_ACTIVITY_INVALID: &str = "该蓝图不能用于这项活动。";
/// Run count exceeds blueprint limit.
pub(crate) const BLUEPRINT_RUN_COUNT_EXCEEDED: &str = "流程数超出了蓝图允许的范围。";
/// Facility outside remote industry range.
pub(crate) const FACILITY_OUTSIDE_REMOTE_INDUSTRY_RANGE: &str = "该设施超出了你的远程工业范围。";
/// Project already delivered.
pub(crate) const INDUSTRY_PROJECT_ALREADY_DELIVERED: &str = "该项目已经交付。";
/// Project not complete.
pub(crate) const INDUSTRY_PROJECT_NOT_COMPLETE: &str = "该项目尚未完成。";
/// No project to install.
pub(crate) const NO_PROJECT_TO_INSTALL: &str = "没有要安装的项目。";
/// Project cannot be cancelled.
pub(crate) const INDUSTRY_PROJECT_CANNOT_CANCEL: &str = "该项目无法取消。";
/// Skill required by an industry project.
pub(crate) fn required_skill_message(skill: &i32, level: &i32) -> String { format!("该项目需要技能 {skill} 达到 {level} 级。") }
/// Industry activity slots are occupied.
pub(crate) fn industry_slots_full_message(slots: i64) -> String { format!("你在这项活动上的全部 {slots} 个工作槽位都已占用。") }
/// Materials required in the facility hangar.
pub(crate) fn required_materials_message(q: &i64, t: &i32) -> String { format!("你需要在此处物品机库中准备 {q} 个类型 {t}。") }

// Insurance (services/insurance.rs)
/// Only own assembled ship can be insured.
pub(crate) const INSURANCE_REQUIRES_OWN_ASSEMBLED_SHIP: &str = "只能为你自己已组装的舰船投保。";
/// Insurance quote expired.
pub(crate) const INSURANCE_QUOTE_EXPIRED: &str = "保险报价已过期，请重新打开窗口。";

// Inventory (services/inventory.rs)
/// No permission to take from hangar division.
pub(crate) const NO_CORPORATION_HANGAR_TAKE_ROLE: &str = "你没有从该军团机库部门取出物品的职权。";
/// Not your corporation hangar.
pub(crate) const NOT_YOUR_CORPORATION_HANGAR: &str = "这不是你军团的机库。";
/// Locked container needs data or relic analyzer.
pub(crate) const CONTAINER_LOCKED_NEEDS_ANALYZER: &str = "这个容器被锁住了。用数据分析仪或遗迹分析仪破解它。";

// Mail (services/mail.rs)
/// Mailing lists unsupported.
pub(crate) const MAILING_LISTS_UNSUPPORTED: &str = "暂不支持邮件列表。";
/// At least one recipient required.
pub(crate) const MAIL_RECIPIENT_REQUIRED: &str = "请至少选择一名收件人。";
/// Too many recipients.
pub(crate) const TOO_MANY_MAIL_RECIPIENTS: &str = "收件人过多。";
/// Mail only to own corporation.
pub(crate) const MAIL_ONLY_TO_OWN_CORPORATION: &str = "你只能给自己的军团发送邮件。";
/// Label limit reached.
pub(crate) const LABEL_LIMIT_REACHED: &str = "标签数量已达上限。";

// Market (services/market.rs)
/// Invalid price.
pub(crate) const INVALID_PRICE: &str = "价格无效。";
/// No market at location.
pub(crate) const NO_MARKET_AT_LOCATION: &str = "该地点没有市场。";
/// Order is closed.
pub(crate) const MARKET_ORDER_CLOSED: &str = "该订单已关闭。";
/// Sell item from its station item hangar.
pub(crate) const MARKET_SELL_ITEM_MUST_BE_IN_STATION_HANGAR: &str = "只能在物品所在空间站出售你物品机库中的物品。";
/// Open market order limit.
pub(crate) fn market_order_limit_message(open: i64) -> String { format!("你已有 {open} 个未完成的市场订单，达到了交易技能允许的上限。") }

// Mining (services/mining.rs)
/// Target outside mining range.
pub(crate) const TARGET_OUTSIDE_MINING_RANGE: &str = "目标超出了采矿设备的范围。";
/// Mining hold full.
pub(crate) const MINING_HOLD_FULL: &str = "你的采矿舱已经满了。";
/// Cargo hold full.
pub(crate) const CARGO_HOLD_FULL: &str = "你的货柜舱已经满了。";
/// Reprocessed output exceeds stack capacity.
pub(crate) const REPROCESS_OUTPUT_STACK_LIMIT: &str = "精炼产物数量超过物品堆叠容量。";
/// Must dock to reprocess.
pub(crate) const MUST_DOCK_TO_REPROCESS: &str = "你必须停靠在空间站里才能进行精炼。";
/// Insufficient ISK for reprocessing tax.
pub(crate) const INSUFFICIENT_ISK_FOR_REPROCESSING_TAX: &str = "你的 ISK 余额不足以支付精炼税。";

// Missions (services/missions.rs)
/// Target cleared; complete and claim reward.
pub(crate) const MISSION_TARGET_CLEARED_COMPLETE: &str = "你已经清除了目标。完成任务，领取你的报酬吧。";
/// Ready; complete and claim reward.
pub(crate) const MISSION_READY_COMPLETE: &str = "一切就绪。完成任务，领取你的报酬吧。";
/// Find named person and deliver there.
pub(crate) const MISSION_FIND_AGENT_DELIVER: &str = "去找我说的那个人吧，完成后在那里交付。";
/// Bring cargo to station hangar, then return.
pub(crate) const MISSION_DELIVER_TO_STATION_HANGAR: &str = "把货物带到这个空间站的机库里，然后再来找我。";
/// Unfinished business remains.
pub(crate) const MISSION_INCOMPLETE: &str = "我们还有事情没办完。";
/// Target remains; report after completion.
pub(crate) const MISSION_TARGET_REMAINS: &str = "目标还在那里。完成后回来向我报告。";
/// Well done.
pub(crate) const MISSION_WELL_DONE: &str = "干得好。";
/// Waiting for pilot.
pub(crate) const MISSION_WAITING_FOR_PILOT: &str = "我一直在等你。";
/// No mission currently available.
pub(crate) const NO_MISSION_AVAILABLE: &str = "我目前没有可以交给你的任务。";
/// Greeting with available work.
pub(crate) const MISSION_GREETING_WITH_WORK: &str = "你好，飞行员。我这里有些工作需要人手。";
/// Nothing for pilot currently.
pub(crate) const NO_CURRENT_MISSION: &str = "现在我这里没有你的事。";
/// Mission accepted confirmation.
pub(crate) const MISSION_ACCEPTED_CONFIRMATION: &str = "就这么定了。";
/// Destination logged after acceptance.
pub(crate) const MISSION_ACCEPTED_DESTINATION_LOGGED: &str = "很好。目标位置已经加入你的日志，出发吧。";
/// Mission declined.
pub(crate) const MISSION_DECLINED: &str = "好吧，也许下次吧。";
/// Mission reward delivery.
pub(crate) const MISSION_REWARD_DELIVERY: &str = "干得漂亮！这是你的报酬。";
/// Mission quit message.
pub(crate) const MISSION_QUIT: &str = "真遗憾。也许下次你能做到。";
/// Pilot greeting.
pub(crate) const PILOT_GREETING: &str = "你好，飞行员。";
/// Agent standing requirement.
pub(crate) fn mission_standing_requirement_message(needed: f64, have: f64) -> String { format!("我只和信誉良好的飞行员合作。你需要对我、我的军团或势力拥有至少 {needed:.1} 的有效声望（目前为 {have:.2}）。") }

// Offices (services/offices.rs)
/// NPC corporations cannot rent offices.
pub(crate) const NPC_CORPORATION_CANNOT_RENT_OFFICE: &str = "NPC 军团不能租用办公室。";
/// No permission to rent office.
pub(crate) const NO_OFFICE_RENTAL_ROLE: &str = "你没有租用办公室的职权。";
/// Office rent changed; query again.
pub(crate) const OFFICE_RENT_CHANGED: &str = "办公室租金已变动，请重新查询。";
/// No permission to release office.
pub(crate) const NO_OFFICE_RELEASE_ROLE: &str = "你没有退租办公室的职权。";
/// Empty hangar before office release.
pub(crate) const EMPTY_CORPORATION_HANGAR_BEFORE_RELEASE: &str = "请先清空军团机库再退租办公室。";

// Organizations (services/orgs.rs)
/// Insufficient role for operation.
pub(crate) const INSUFFICIENT_ROLE_FOR_OPERATION: &str = "你没有执行此操作所需的职权。";

// Ship (services/ship.rs)
/// Must be in space.
pub(crate) const MUST_BE_IN_SPACE: &str = "你必须在太空中。";
/// Weapons timer prevents an action.
pub(crate) fn weapons_timer_message(secs: i64, what: &str) -> String { format!("武器计时器仍在生效，{secs} 秒内无法{what}。") }
/// Docking action in a weapons timer refusal.
pub(crate) const WEAPONS_TIMER_DOCK: &str = "停靠";
/// Jumping action in a weapons timer refusal.
pub(crate) const WEAPONS_TIMER_JUMP: &str = "跳跃";

// Standings (services/standings.rs)
/// Loyalty-point store requires docking.
pub(crate) const STATION_DOCK_REQUIRED_FOR_LP_STORE: &str = "你必须停靠在空间站内才能使用忠诚点商店。";
/// Loyalty-point offer is unavailable.
pub(crate) const LP_OFFER_UNAVAILABLE: &str = "该兑换项已不可用。";
/// Available loyalty point balance.
pub(crate) fn loyalty_balance_message(before: i64) -> String { format!("你在该军团只有 {before} 忠诚点。") }

// Station (services/station.rs)
/// Amplification matrix is unavailable.
pub(crate) const AMPLIFICATION_MATRIX_UNAVAILABLE: &str = "增幅矩阵暂不可用。";

// Store (services/store.rs)
/// Daily gift category.
pub(crate) const DAILY_GIFT_CATEGORY_NAME: &str = "每日礼包";
/// Ship skin category.
pub(crate) const SHIP_SKIN_CATEGORY_NAME: &str = "舰船涂装";
/// Skills and services category.
pub(crate) const SKILLS_AND_SERVICES_CATEGORY_NAME: &str = "技能与服务";
/// Apparel category; includes SQL fallback.
pub(crate) const APPAREL_CATEGORY_NAME: &str = "服饰";
/// Supply crate category.
pub(crate) const SUPPLY_CRATE_CATEGORY_NAME: &str = "补给箱";
/// Daily free subcategory.
pub(crate) const DAILY_FREE_CATEGORY_NAME: &str = "每日免费";
/// Skill injector subcategory.
pub(crate) const SKILL_INJECTOR_CATEGORY_NAME: &str = "技能注入";
/// Amarr category.
pub(crate) const AMARR_CATEGORY_NAME: &str = "艾玛";
/// Caldari category.
pub(crate) const CALDARI_CATEGORY_NAME: &str = "加达里";
/// Gallente category.
pub(crate) const GALLENTE_CATEGORY_NAME: &str = "盖伦特";
/// Minmatar category.
pub(crate) const MINMATAR_CATEGORY_NAME: &str = "米玛塔尔";
/// Other faction category.
pub(crate) const OTHER_FACTION_CATEGORY_NAME: &str = "其他";

// Client message-catalogue names
/// Client catalogue key from clientDogmaLocation.LinkWeapons.
pub(crate) const MSG_CANT_LINK_MODULE_NOT_ONLINE: &str = "CantLinkModuleNotOnline";
/// Client fittingDogmaLocation.py refusal for an activation-blocked module.
pub(crate) const MSG_MODULE_IS_BLOCKED: &str = "ModuleIsBlocked";
/// Client message `BkmMustBeInFlight`.
pub(crate) const MSG_BKM_MUST_BE_IN_FLIGHT: &str = "BkmMustBeInFlight";
/// Client message `BlueprintNotFound`.
pub(crate) const MSG_BLUEPRINT_NOT_FOUND: &str = "BlueprintNotFound";
/// Client message `BookmarkFolderNoLongerThere`.
pub(crate) const MSG_BOOKMARK_FOLDER_NO_LONGER_THERE: &str = "BookmarkFolderNoLongerThere";
/// Client message `CanNotPlaceActiveShipInAnotherHangar`.
pub(crate) const MSG_CAN_NOT_PLACE_ACTIVE_SHIP_IN_ANOTHER_HANGAR: &str = "CanNotPlaceActiveShipInAnotherHangar";
/// Client message `CantActivateReloadingModule`.
pub(crate) const MSG_CANT_ACTIVATE_RELOADING_MODULE: &str = "CantActivateReloadingModule";
/// Client message `CantApplyTwiceToAlliance`.
pub(crate) const MSG_CANT_APPLY_TWICE_TO_ALLIANCE: &str = "CantApplyTwiceToAlliance";
/// Client message `CantAssembleModularShipInSpace`.
pub(crate) const MSG_CANT_ASSEMBLE_MODULAR_SHIP_IN_SPACE: &str = "CantAssembleModularShipInSpace";
/// Client message `CantBoardShipNotInHangar`.
pub(crate) const MSG_CANT_BOARD_SHIP_NOT_IN_HANGAR: &str = "CantBoardShipNotInHangar";
/// Client message `CantBoardShipWhileInSpace`.
pub(crate) const MSG_CANT_BOARD_SHIP_WHILE_IN_SPACE: &str = "CantBoardShipWhileInSpace";
/// Client message `CantBoardUnassembledShip`.
pub(crate) const MSG_CANT_BOARD_UNASSEMBLED_SHIP: &str = "CantBoardUnassembledShip";
/// Client message `CantRepackageActiveShip`.
pub(crate) const MSG_CANT_REPACKAGE_ACTIVE_SHIP: &str = "CantRepackageActiveShip";
/// Client message `CantUndockNotInStation`.
pub(crate) const MSG_CANT_UNDOCK_NOT_IN_STATION: &str = "CantUndockNotInStation";
/// Client message `CharacterNotFound`.
pub(crate) const MSG_CHARACTER_NOT_FOUND: &str = "CharacterNotFound";
/// Client message `CharacterNotReadyForDeletion`.
pub(crate) const MSG_CHARACTER_NOT_READY_FOR_DELETION: &str = "CharacterNotReadyForDeletion";
/// Client message `CharNameInvalid`.
pub(crate) const MSG_CHAR_NAME_INVALID: &str = "CharNameInvalid";
/// Client message `ConContractNotFound`.
pub(crate) const MSG_CON_CONTRACT_NOT_FOUND: &str = "ConContractNotFound";
/// Client message `ConContractNotOutstanding`.
pub(crate) const MSG_CON_CONTRACT_NOT_OUTSTANDING: &str = "ConContractNotOutstanding";
/// Client message `ConfirmRepackageSomethingWithUpgrades`.
pub(crate) const MSG_CONFIRM_REPACKAGE_SOMETHING_WITH_UPGRADES: &str = "ConfirmRepackageSomethingWithUpgrades";
/// Client message `CorpBulletinHasBeenEdited`.
pub(crate) const MSG_CORP_BULLETIN_HAS_BEEN_EDITED: &str = "CorpBulletinHasBeenEdited";
/// Client message `CorpBulletinNotFound`.
pub(crate) const MSG_CORP_BULLETIN_NOT_FOUND: &str = "CorpBulletinNotFound";
/// Client message `CorpTooManyBulletins`.
pub(crate) const MSG_CORP_TOO_MANY_BULLETINS: &str = "CorpTooManyBulletins";
/// Client message `CustomNotify`.
pub(crate) const MSG_CUSTOM_NOTIFY: &str = "CustomNotify";
/// Client kill-right refusal keys.
pub(crate) const MSG_NO_VALID_KILL_RIGHT: &str = "NoValidKillRight";
pub(crate) const MSG_KILL_RIGHT_EXPIRED: &str = "KillRightExpired";
pub(crate) const MSG_KILL_RIGHT_NOT_FOR_SALE: &str = "KillRightNotForSale";
/// Client message `CustomQuestion`.
pub(crate) const MSG_CUSTOM_QUESTION: &str = "CustomQuestion";
/// Client message `DeniedActivateInWarp`.
pub(crate) const MSG_DENIED_ACTIVATE_IN_WARP: &str = "DeniedActivateInWarp";
/// Client message `DeniedActivateTargetNotPresent`.
pub(crate) const MSG_DENIED_ACTIVATE_TARGET_NOT_PRESENT: &str = "DeniedActivateTargetNotPresent";
/// Client message `DockingRequestDenied`.
pub(crate) const MSG_DOCKING_REQUEST_DENIED: &str = "DockingRequestDenied";
/// Client message `FleetError`.
pub(crate) const MSG_FLEET_ERROR: &str = "FleetError";
/// Client message `IndustryBlueprintAccessDenied`.
pub(crate) const MSG_INDUSTRY_BLUEPRINT_ACCESS_DENIED: &str = "IndustryBlueprintAccessDenied";
/// Client message `IndustryJobAccessDenied`.
pub(crate) const MSG_INDUSTRY_JOB_ACCESS_DENIED: &str = "IndustryJobAccessDenied";
/// Client message `IndustryJobNotFound`.
pub(crate) const MSG_INDUSTRY_JOB_NOT_FOUND: &str = "IndustryJobNotFound";
/// Client message `InsCouldNotFindItem`.
pub(crate) const MSG_INS_COULD_NOT_FIND_ITEM: &str = "InsCouldNotFindItem";
/// Client message `InsureShipFailedSingleContract`.
pub(crate) const MSG_INSURE_SHIP_FAILED_SINGLE_CONTRACT: &str = "InsureShipFailedSingleContract";
/// Client message `JumpGateHasNoDestination`.
pub(crate) const MSG_JUMP_GATE_HAS_NO_DESTINATION: &str = "JumpGateHasNoDestination";
/// Client message `MailLabelMustProvideName`.
pub(crate) const MSG_MAIL_LABEL_MUST_PROVIDE_NAME: &str = "MailLabelMustProvideName";
/// Client message `MktOrderDidNotMatch`.
pub(crate) const MSG_MKT_ORDER_DID_NOT_MATCH: &str = "MktOrderDidNotMatch";
/// Client message `NoCharges`.
pub(crate) const MSG_NO_CHARGES: &str = "NoCharges";
/// Client message `NotAtStation`.
pub(crate) const MSG_NOT_AT_STATION: &str = "NotAtStation";
/// Client message `NotCloseEnoughToJump`.
pub(crate) const MSG_NOT_CLOSE_ENOUGH_TO_JUMP: &str = "NotCloseEnoughToJump";
/// Client message `NotEnoughMoney`.
pub(crate) const MSG_NOT_ENOUGH_MONEY: &str = "NotEnoughMoney";
/// Client message `OnlyCEOCanResign`.
pub(crate) const MSG_ONLY_CEO_CAN_RESIGN: &str = "OnlyCEOCanResign";
/// Client message `RepackageBeforeSelling`.
pub(crate) const MSG_REPACKAGE_BEFORE_SELLING: &str = "RepackageBeforeSelling";
/// Client message `ShipNotFound`.
pub(crate) const MSG_SHIP_NOT_FOUND: &str = "ShipNotFound";
/// Client message `SlashError`.
pub(crate) const MSG_SLASH_ERROR: &str = "SlashError";
/// Client message `TargetingAttemptCancelled`.
pub(crate) const MSG_TARGETING_ATTEMPT_CANCELLED: &str = "TargetingAttemptCancelled";
