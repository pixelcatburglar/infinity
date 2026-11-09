//! Player-visible market refusals and batch summaries.

pub const CORPORATION_OPEN_ORDERS: &str = "公司仍有未完成的市场订单，无法解散。";
pub const REMOTE_TRADE_OUT_OF_RANGE: &str = "目标空间站超出你的远程交易技能范围。";
pub const ORDER_MODIFICATION_COOLDOWN: &str = "订单修改冷却时间尚未结束。";

pub fn sell_failures(failures: &[(i64, i64, i64)]) -> String {
    let details = failures.iter().map(|(item, kind, quantity)| format!("物品 {item}（类型 {kind}），未成交数量 {quantity}")).collect::<Vec<_>>().join("；");
    format!("部分物品出售失败：{details}")
}
