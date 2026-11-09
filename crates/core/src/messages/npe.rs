//! Player-visible refusals for unmodelled new-player features.
//! Operations uses no data and does not implement tutorial state or actions.

/// Operations tutorial gameplay is unavailable.
pub const OPERATIONS_UNAVAILABLE: &str = "行动教程暂不可用。";

/// The unsupported QA level editor cannot be opened.
pub const LEVEL_EDITOR_UNAVAILABLE: &str = "本服务器暂不提供关卡编辑器。";
/// Tutorial scene positions are unavailable.
pub const OPERATION_SITE_UNAVAILABLE: &str = "本服务器暂不提供新手教程场景。";

/// Server node graph QA controls are unavailable.
pub const NODE_GRAPH_UNAVAILABLE: &str = "本服务器暂不提供节点图调试控制。";
