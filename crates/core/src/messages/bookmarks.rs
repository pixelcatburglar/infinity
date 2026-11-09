//! Player-visible refusals for bookmark folder access, validation, and limits.
//! Supports `eve/client/script/ui/shared/bookmarks/bookmarkSvc.py` operations.
//! Text is server-authored and uses no database or other data source.
//! Does not implement localisation or client message-catalogue keys.

/// Bookmark operation requires folder access.
pub const BOOKMARK_ACCESS_DENIED: &str = "你没有访问此书签文件夹的权限。";
/// Bookmark destination or parameters are invalid.
pub const BOOKMARK_INVALID_TARGET: &str = "书签目标无效。";
/// Bookmark or folder limit reached.
pub const BOOKMARK_LIMIT_REACHED: &str = "已达到书签数量上限。";
/// Bookmark expiry is invalid.
pub const BOOKMARK_INVALID_EXPIRY: &str = "书签过期时间无效。";
