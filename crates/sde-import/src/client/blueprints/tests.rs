use super::*;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("sde-blueprint #{}-{}.sqlite", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        let db = Connection::open(&path).unwrap();
        db.execute_batch("CREATE TABLE cache (key TEXT PRIMARY KEY, value TEXT, time REAL);").unwrap();
        Self(path)
    }
    fn insert(&self, key: &str, json: &str) { let db = Connection::open(&self.0).unwrap(); db.execute("INSERT INTO cache (key,value) VALUES (?1,?2)", [key,json]).unwrap(); }
}
impl Drop for Fixture { fn drop(&mut self) { let _ = std::fs::remove_file(&self.0); let _ = std::fs::remove_file(sibling(&self.0, "-wal")); let _ = std::fs::remove_file(sibling(&self.0, "-journal")); } }

#[test]
fn reads_rows_from_escaped_path() {
    let f = Fixture::new(); f.insert("7", r#"{"blueprintTypeID":7,"activities":{"invent":[]}}"#);
    let rows = decode(&f.0).unwrap(); assert_eq!(rows.records[0]["_key"], 7); assert_eq!(rows.records[0]["activities"]["invent"], serde_json::json!([]));
    assert!(uri(&f.0).unwrap().contains("%23"));
}

#[test]
fn encodes_absolute_paths_for_sqlite() {
    for (path, windows, expected) in [
        ("/a b/#?%/é.db", false, "file:/a%20b/%23%3F%25/%C3%A9.db?immutable=1"),
        (r"C:\dir #\é.db", true, "file:/C:/dir%20%23/%C3%A9.db?immutable=1"),
        ("d:/dir/a:b.db", true, "file:/d:/dir/a%3Ab.db?immutable=1"),
        (r"\\?\C:\dir\x.db", true, "file:/C:/dir/x.db?immutable=1"),
        (r"\\server\share\x.db", true, "file:////server/share/x.db?immutable=1"),
        (r"\\?\UNC\server\share\x.db", true, "file:////server/share/x.db?immutable=1"),
    ] {
        assert_eq!(uri_from_bytes(path.as_bytes(), windows).unwrap(), expected, "{path}");
    }
}

#[test]
fn rejects_relative_and_device_paths() {
    for (path, windows) in [
        ("relative/file", false), (r"C:relative", true), (r"\rooted", true),
        (r"\\.\PhysicalDrive0", true), (r"\\?\Volume{abc}\x", true),
        (r"\??\C:\x", true), (r"\\server", true),
    ] {
        assert!(uri_from_bytes(path.as_bytes(), windows).is_err(), "{path}");
    }
}

#[cfg(unix)]
#[test]
fn encodes_non_utf8_path() {
    use std::os::unix::ffi::OsStrExt;
    assert_eq!(uri(Path::new(std::ffi::OsStr::from_bytes(b"/invalid\xff #"))).unwrap(), "file:/invalid%FF%20%23?immutable=1");
}

#[cfg(windows)]
#[test]
fn rejects_unpaired_surrogate_path() {
    use std::os::windows::ffi::OsStringExt;
    let path = std::ffi::OsString::from_wide(&[b'C' as u16, b':' as u16, b'\\' as u16, 0xD800]);
    assert!(uri(Path::new(&path)).is_err());
}

#[test]
fn preserves_exact_json_numbers() {
    let f = Fixture::new();
    f.insert("7", r#"{"quantity":9007199254740993,"fraction":0.10000000000000001}"#);
    let decoded = decode(&f.0).unwrap();
    assert!(decoded.raw_records[0].contains("9007199254740993"));
    assert!(decoded.raw_records[0].contains("0.10000000000000001"));
}

#[test]
fn checked_numeric_projection_preserves_raw_tokens() {
    let f = Fixture::new();
    f.insert("7", r#"{"a":1.0,"b":1e0,"max":1.7976931348623157e308,"zero":-0,"small":-0.0,"large":18446744073709551615}"#);
    let decoded = decode(&f.0).unwrap();
    let row = &decoded.records[0];
    assert_eq!(row["a"].as_f64(), row["b"].as_f64());
    assert_eq!(row["max"].as_f64(), Some(f64::MAX));
    assert_eq!(row["zero"].as_f64().unwrap().to_bits(), (-0.0f64).to_bits());
    assert_eq!(row["small"].as_f64().unwrap().to_bits(), (-0.0f64).to_bits());
    assert_eq!(row["large"].as_u64(), Some(u64::MAX));
    assert!(decoded.raw_records[0].contains("1e0"));
    assert!(decoded.raw_records[0].contains("-0.0"));
}

#[test]
fn rejects_unrepresentable_and_malformed_numbers() {
    for payload in [
        r#"{"x":18446744073709551616}"#, r#"{"x":-9223372036854775809}"#,
        r#"{"x":1e309}"#, r#"{"x":Infinity}"#, r#"{"x":1.}"#,
        r#"{"blueprintTypeID":7.0}"#, r#"{"blueprintTypeID":"7"}"#,
        r#"{"nested":{"a":1,"a":2}}"#,
    ] {
        let f = Fixture::new(); f.insert("7", payload); assert!(decode(&f.0).is_err(), "{payload}");
    }
}
#[test]
fn rejects_invalid_rows_and_key_collisions() {
    for payload in [r#"{"a":1,"a":2}"#, r#"{"blueprintTypeID":8}"#, r#"{"x":NaN}"#, r#"{"_key":7}"#] {
        let f = Fixture::new(); f.insert("7", payload); assert!(decode(&f.0).is_err());
    }
    let f = Fixture::new(); f.insert("7", "{}"); f.insert("07", "{}"); assert!(decode(&f.0).is_err());
}
#[test]
fn rejects_wal_and_journal() {
    let f = Fixture::new(); std::fs::write(sibling(&f.0, "-wal"), b"x").unwrap(); assert!(decode(&f.0).is_err());
    std::fs::remove_file(sibling(&f.0, "-wal")).unwrap(); std::fs::write(sibling(&f.0, "-journal"), b"").unwrap(); assert!(decode(&f.0).is_err());
}
