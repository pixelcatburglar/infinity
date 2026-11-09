use super::{decode, tagged_json, Graph, Node, MAX_BINARY_LONG, MAX_BYTES, MAX_DEPTH, MAX_SCALAR};
use serde_json::json;

#[test]
fn nested_values_and_tags() {
    let g = decode(b"\x80\x02}q\x00(X\x01\x00\x00\x00aK\x07X\x01\x00\x00\x00b]q\x01(K\x01K\x02eu.").unwrap();
    assert_eq!(tagged_json(&g).unwrap(), json!({"$dict":[["a",7],["b",[1,2]]]}));
    let g = decode(b"(S'plain'\nV\\u4e2d\nt.").unwrap();
    assert_eq!(tagged_json(&g).unwrap(), json!({"$tuple":["plain","中"]}));
}

#[test]
fn memo_mutation_and_ordered_dict() {
    let g = decode(b"\x80\x02]q\x00(]q\x01h\x01K\x05ae.").unwrap();
    assert_eq!(tagged_json(&g).unwrap(), json!([[5],[5]]));
    let Node::List(root) = &g.nodes[g.root] else { panic!() }; assert_eq!(root[0], root[1]); assert!(matches!(&g.nodes[root[0]], Node::List(v) if v.len() == 1));
    let g = decode(b"ccollections\nOrderedDict\n)R(S'a'\nI1\nu.").unwrap();
    assert_eq!(tagged_json(&g).unwrap(), json!({"$dict":[["a",1]]}));
}

#[test]
fn legacy_strings_are_text_with_pickletools_encodings() {
    let input = b"(S'quote\\' slash\\\\'\nT\x03\0\0\0A\x80\xffU\x03\0\xe9\xffV\\u4e2d\nX\x03\0\0\0\xe4\xb8\xadt.";
    assert_eq!(tagged_json(&decode(input).unwrap()).unwrap(), json!({"$tuple":["quote' slash\\","A\u{80}ÿ","\0éÿ","中","中"]}));
    assert!(decode(b"S'\\xff'\n.").is_err());
    assert!(decode(b"S'\xff'\n.").is_err());
    assert!(decode(b"B\x01\0\0\0a.").is_err());
    // Distinct opcode encodings of the same text are duplicate Python keys.
    assert!(decode(b"}(S'a'\nNT\x01\0\0\0aNu.").is_err());
    assert!(super::decode_with_limits(b"U\x01\xff.", (1, 0, 2)).is_ok());
    assert_eq!(super::decode_with_limits(b"U\x01\xff.", (1, 0, 1)).unwrap_err(), "graph scalar limit");
}

#[test]
fn big_integer_and_signed_long() {
    let g = decode(b"(L18446744073709551616L\n\x8a\x02\xff\x00\x8a\x01\xfft.").unwrap();
    assert_eq!(tagged_json(&g).unwrap(), json!({"$tuple":[{"$int":"18446744073709551616"},255,-1]}));
}

#[test]
fn rejects_unsafe_malformed_and_duplicates() {
    for input in [b"cos\nsystem\n)R.".as_slice(), b"N)R.", b"\x80\x04N.", b"\x95N.", b"N.x", b"N", b"Fnan\n.", b"G\x7f\xf0\x00\x00\x00\x00\x00\x00.", b"(dI1\nI2\nsI01\nI3\ns.", b"]q\x00h\x00a.", b"}([]I1\ns.", b"h\x00.", b"(K\x01d."] {
        assert!(decode(input).is_err(), "accepted {input:?}");
    }
}

#[test]
fn bounds_and_depth() {
    assert!(decode(&vec![0; MAX_BYTES + 1]).is_err());
    let mut scalar = vec![b'X']; scalar.extend_from_slice(&((MAX_SCALAR + 1) as u32).to_le_bytes()); assert!(decode(&scalar).is_err());
    let mut nested = vec![b']'; MAX_DEPTH + 1]; nested.extend(std::iter::repeat(b'a').take(MAX_DEPTH)); nested.push(b'.');
    assert!(decode(&nested).is_err());
}

#[test]
fn binary_integer_and_shared_json_budgets() {
    let mut input = vec![0x8b];
    input.extend_from_slice(&(MAX_BINARY_LONG as u32).to_le_bytes());
    input.extend(vec![0; MAX_BINARY_LONG]);
    input.push(b'.');
    assert_eq!(tagged_json(&decode(&input).unwrap()).unwrap(), json!(0));
    let mut oversized = vec![0x8b];
    oversized.extend_from_slice(&((MAX_BINARY_LONG + 1) as u32).to_le_bytes());
    assert_eq!(decode(&oversized).unwrap_err(), "binary integer size limit");
    let graph = Graph { root: 1, nodes: vec![Node::Text("x".repeat(1024 * 1024)), Node::List(vec![0; 256])] };
    assert_eq!(tagged_json(&graph).unwrap_err(), "JSON expansion byte limit");
}

#[test]
fn sequential_setitems_and_duplicate_across_batches() {
    let mut input = vec![b'}'];
    for i in 0..20_000 { input.extend_from_slice(format!("I{i}\nNs").as_bytes()); }
    input.push(b'.');
    let graph = decode(&input).unwrap();
    assert!(matches!(&graph.nodes[graph.root], Node::Dict(v) if v.len() == 20_000));
    assert!(decode(b"}I7\nNsI7\nNs.").is_err());
    assert!(decode(b"}I7\nNs(I8\nNI7\nNu.").is_err());
    assert!(decode(b"}(I7\nNuI7\nNs.").is_err());
}

#[test]
fn accumulated_graph_budgets() {
    // Each completed child has one scalar and is appended to the outer list.
    // Only three stack entries and one item per child are live at a time.
    let mut input = vec![b']'];
    for _ in 0..1000 { input.extend_from_slice(b"]Naa"); }
    input.push(b'.');
    let limits = (2001, 2000, 0);
    assert!(super::decode_with_limits(&input, limits).is_ok());
    assert_eq!(super::decode_with_limits(&input, (2000, 2000, 0)).unwrap_err(), "graph node limit");
    assert_eq!(super::decode_with_limits(&input, (2001, 1999, 0)).unwrap_err(), "graph edge limit");
    assert!(super::decode_with_limits(b"(X\x03\0\0\0abcX\x03\0\0\0deft.", (3, 2, 6)).is_ok());
    assert_eq!(super::decode_with_limits(b"(X\x03\0\0\0abcX\x03\0\0\0deft.", (3, 2, 5)).unwrap_err(), "graph scalar limit");
}
