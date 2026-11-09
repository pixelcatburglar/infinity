//! Console room identifiers and lossless rendered text boundaries.
use super::*;

fn split_text(text: &str) -> Vec<&str> {
    let mut chunks = Vec::new();
    visit_chunks(text, |chunk| chunks.push(chunk));
    chunks
}

#[test]
fn canonical_rooms_round_trip_only_positive_recipients() {
    assert_eq!(recipient(&room(42)), Some(42));
    for invalid in ["private_1_0", "private_1_-2", "private_01_42", "private_1_042", "private_1_+42", "private_2_42", "private_1_42/x", "private_1_42_3"] {
        assert_eq!(recipient(invalid), None, "{invalid}");
    }
}

#[test]
fn chunks_preserve_text_and_bound_rendered_utf16() {
    for text in [format!("{}\n{}", "a".repeat(1020), "舰🚀<&\"'".repeat(600)), "🚀".repeat(1024), "&".repeat(1024), "\n".repeat(1024)] {
        let chunks = split_text(&text);
        assert_eq!(chunks.concat(), text);
        assert!(chunks.iter().all(|chunk| !chunk.is_empty() && rendered(chunk).encode_utf16().count() <= MAX_TEXT_UTF16));
    }
    let text = format!("{}\n{}", "a".repeat(100), "b".repeat(1100));
    assert!(split_text(&text)[0].ends_with('\n'));
    assert!(split_text("").is_empty());
}

#[test]
fn early_newline_and_expanding_scalar_never_overflow_suffix() {
    for text in [
        format!("\n{}\"", "a".repeat(1019)),
        format!("\n{}🚀", "a".repeat(1023)),
        format!("\n{}&", "🚀".repeat(510)),
        format!("\n{}'\n<", "a".repeat(1019)),
    ] {
        let chunks = split_text(&text);
        assert_eq!(chunks.concat(), text);
        assert!(chunks.iter().all(|chunk| !chunk.is_empty() && rendered(chunk).encode_utf16().count() <= MAX_TEXT_UTF16));
    }
    let text = format!("\n{}\"", "a".repeat(1019));
    assert_eq!(split_text(&text), vec!["\n", &text[1..1020], "\""]);
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with initialized SDE"]
async fn private_reply_requires_current_token_and_online_session() {
    use infinity_core::session::OnlineSession;
    use crate::{db::Db, sde::Sde, space::World};
    use std::sync::Arc;

    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let world = World::new(db.clone(), sde, 1);
    let hub = Hub::new("chat.test", db, world.clone());
    let (pilot_tx, _pilot_rx) = tokio::sync::mpsc::unbounded_channel();
    world.register_pilot(42, pilot_tx);
    world.set_online_session(42, Some(OnlineSession {
        character_id: 42, user_id: None, session_id: 1, ship_id: None, station_id: None,
        system_id: None, corporation_id: None, alliance_id: None, faction_id: None,
        fleet_id: None, role: 0, corporation_role: 0,
    }));
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    hub.users.lock().unwrap().insert("42@chat.test".into(), ("42@chat.test/test".into(), tx.clone()));
    hub.token_for(42);
    hub.send_system_private(42, "online");
    assert!(rx.try_recv().is_ok());
    while rx.try_recv().is_ok() {}
    hub.disconnect_character(42);
    assert!(!hub.users.lock().unwrap().contains_key("42@chat.test"));
    hub.send_system_private(42, "revoked");
    assert!(rx.try_recv().is_err());
    assert!(!hub.rooms.lock().unwrap().entries.contains_key(&format!("{}@conference.chat.test", room(42))));
    hub.token_for(42);
    hub.send_system_private(42, "new token without bind");
    assert!(rx.try_recv().is_err());
    assert!(!hub.rooms.lock().unwrap().entries.contains_key(&format!("{}@conference.chat.test", room(42))));
    world.unregister_pilot_generation(42, None);
    hub.users.lock().unwrap().insert("42@chat.test".into(), ("42@chat.test/test".into(), tx));
    hub.send_system_private(42, "offline");
    assert!(rx.try_recv().is_err());
    assert!(!hub.rooms.lock().unwrap().entries.contains_key(&format!("{}@conference.chat.test", room(42))));
}

#[test]
fn rendering_escapes_markup_and_preserves_line_breaks() {
    assert_eq!(rendered("<&\n>"), "&lt;&amp;<br>&gt;");
}

#[test]
fn escaping_and_rendered_size_share_all_entities() {
    let text = "&<>'\"🚀\n舰";
    assert_eq!(esc(text), "&amp;&lt;&gt;&apos;&quot;🚀\n舰");
    assert_eq!(rendered(text), "&amp;&lt;&gt;&apos;&quot;🚀<br>舰");
    for ch in text.chars() {
        assert_eq!(rendered_units(ch), rendered(&ch.to_string()).encode_utf16().count(), "{ch:?}");
    }
}
