//! Immutable English and Chinese text used by dungeon, store and goal metadata.
//! This server-startup dataset has no direct client mirror.
//! Sources: native localization, dungeons, types, market groups and contribution methods. Other languages and unreferenced text are unsupported.

use crate::{dao, db::Db};

pub(crate) struct LocalizedText {
    pub id: i64,
    pub text: Option<String>,
}

#[cfg_attr(test, derive(Default))]
pub(crate) struct Localization {
    english: Vec<LocalizedText>,
    chinese: Vec<LocalizedText>,
}

impl Localization {
    pub(crate) async fn load(db: &Db) -> anyhow::Result<Self> {
        let (english, chinese) = tokio::try_join!(dao::sde::localization::english(db), dao::sde::localization::chinese(db))?;
        Ok(Self { english, chinese })
    }

    pub(crate) fn english(&self, id: i64) -> Option<&str> {
        self.english.binary_search_by_key(&id, |row| row.id).ok().and_then(|index| self.english[index].text.as_deref())
    }

    pub(crate) fn chinese(&self, id: i64) -> Option<&str> {
        self.chinese.binary_search_by_key(&id, |row| row.id).ok().and_then(|index| self.chinese[index].text.as_deref())
    }

    pub(crate) fn len(&self) -> usize { self.english.len() + self.chinese.len() }
}
