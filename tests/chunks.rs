#![cfg(feature = "std")]
use serde::Deserialize;
use serde_json::{Deserializer, Value};
fn parse<'a, T: Deserialize<'a>>(chunks: &[&'a [u8]]) -> serde_json::Result<T> {
    let mut de = Deserializer::from_chunks(chunks.iter().copied());
    let value = T::deserialize(&mut de)?;
    de.end()?;
    Ok(value)
}
#[test]
fn every_split_matches_slice_parser() {
    let cases: &[&[u8]] = &[
        br#"{"text":"hello","escaped":"\"\\\n\u4e2d\uD83D\uDE00","number":-1.25e3,"nested":[null,true,{}]}"#,
        "{\"text\":\"中文🙂\"}".as_bytes(),
        b" \n [1234567890123456789,0.0123456789,-1.0e-20] \n ",
        br#"{"ignored":"abc\uD83D\uDE00"}"#,
        b"", b"{", b"[1,]", b"true false", b"\"\xff\"", b"\"control\n\"",
        br#""\uD800""#, br#""\uDC00""#, br#""\uD800\u0000""#,
        br#""\u00xy""#, br#""\x""#, br#""incomplete\"#, b"1e", b"-", b"01",
    ];
    for input in cases {
        for split in 0..=input.len() {
            let chunks = [&input[..split], &[], &input[split..]];
            let expected = serde_json::from_slice::<Value>(input);
            let actual = parse::<Value>(&chunks);
            match (expected, actual) {
                (Ok(a), Ok(b)) => assert_eq!(a, b),
                (Err(a), Err(b)) => assert_eq!(
                    (a.classify(), a.line(), a.column()),
                    (b.classify(), b.line(), b.column()),
                    "input {input:?}, split {split}: {a} / {b}"
                ),
                (a, b) => panic!("mismatch {input:?} split {split}: {a:?} / {b:?}"),
            }
        }
        let single_bytes: Vec<_> = input.chunks(1).collect();
        assert_eq!(
            serde_json::from_slice::<Value>(input).ok(),
            parse::<Value>(&single_bytes).ok()
        );
    }
}
#[test]
fn strings_borrow_the_original_frame_and_spanning_owned_strings_decode() {
    let first = br#"["first","#;
    let second = br#""second"]"#;
    let strings: Vec<&str> = parse(&[first, second]).unwrap();
    assert!(first.as_ptr_range().contains(&strings[0].as_ptr()));
    assert!(second.as_ptr_range().contains(&strings[1].as_ptr()));
    for input in [r#""long string""#, r#""escape: \uD83D\uDE00""#] {
        for split in 0..=input.len() {
            let text: String =
                parse(&[&input.as_bytes()[..split], &input.as_bytes()[split..]]).unwrap();
            assert_eq!(text, serde_json::from_str::<String>(input).unwrap());
        }
    }
}
#[test]
fn ignored_strings_and_stream_offsets_cross_chunks() {
    #[derive(Deserialize, Debug, PartialEq)]
    struct Field {
        keep: u32,
    }
    let input = br#"{"skip":"ab\uD83D\uDE00cd","keep":42}"#;
    for split in 0..=input.len() {
        assert_eq!(
            parse::<Field>(&[&input[..split], &input[split..]]).unwrap(),
            Field { keep: 42 }
        );
    }
    let chunks = [b"12 ".as_slice(), b"true\n{}"];
    let mut stream = Deserializer::from_chunks(chunks.into_iter()).into_iter::<Value>();
    assert_eq!(stream.next().unwrap().unwrap(), 12);
    assert_eq!(stream.byte_offset(), 2);
    assert_eq!(stream.next().unwrap().unwrap(), true);
    assert_eq!(stream.byte_offset(), 7);
    assert!(stream.next().unwrap().unwrap().is_object());
    assert_eq!(stream.byte_offset(), 10);
    assert!(stream.next().is_none());
    assert!(stream.next().is_none());
}
#[cfg(feature = "raw_value")]
#[test]
fn raw_values_borrow_or_materialize_only_the_requested_span() {
    use serde_json::value::RawValue;
    let input = br#" {"a":["x",{"b":true}]} "#;
    for split in 0..=input.len() {
        let raw: Box<RawValue> = parse(&[&input[..split], &input[split..]]).unwrap();
        assert_eq!(raw.get(), std::str::from_utf8(input).unwrap().trim());
    }
    let raw: &RawValue = parse(&[input]).unwrap();
    assert!(input.as_ptr_range().contains(&raw.get().as_ptr()));
}
