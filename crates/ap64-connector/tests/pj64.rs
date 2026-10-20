//! The Project64 memory-bridge stand-in, spoken to as Pokémon Snap's client speaks to the
//! script it replaces (`pj64_connector.py`: `<id>:<command>`, addresses from Python's `hex()`,
//! values from `json.dumps`), against a RAM image instead of a console.

use std::cell::RefCell;
use std::rc::Rc;

use ap64_cart::backend::RamImage;
use ap64_connector::{Connector, PJ64};

fn connector() -> (Connector, Rc<RefCell<RamImage>>) {
    let mut ram = vec![0u8; 0x80_0000];
    // gExpansionMagic 'OKAY' at 0x80400000, and the auth token at 0x804004C4.
    ram[0x40_0000..0x40_0004].copy_from_slice(b"OKAY");
    ram[0x40_04C4..0x40_04D4].copy_from_slice(b"0123456789abcdef");
    // A species score, s16 -2 then 300, at 0x8040004C.
    ram[0x40_004C..0x40_0050].copy_from_slice(&[0xFF, 0xFE, 0x01, 0x2C]);
    let mut rom = vec![0u8; 0x1000];
    rom[..4].copy_from_slice(&[0x80, 0x37, 0x12, 0x40]);
    rom[0x10..0x18].copy_from_slice(&[0xCA, 0x12, 0xB5, 0x47, 0x71, 0xFA, 0x4E, 0xE4]);
    rom[0x20..0x34].copy_from_slice(b"POKEMON SNAP        ");
    rom[0x3B..0x3F].copy_from_slice(b"NPFE");
    let ram = Rc::new(RefCell::new(RamImage::with_rom(ram, rom)));
    let c = Connector::new(&PJ64, Box::new(ram.clone()), ap64_cart::quiet()).unwrap();
    (c, ram)
}

#[test]
fn answers_the_reads_the_client_makes() {
    let (c, _) = connector();
    // _expansion_loaded: one u32, read as a list.
    assert_eq!(
        c.handle("1:read u32 0x80400000 4").unwrap(),
        "1:[1330331993]"
    );
    // _read_auth_token: 16 u8s.
    assert_eq!(
        c.handle("2:read u8 0x804004c4 16").unwrap(),
        "2:[48,49,50,51,52,53,54,55,56,57,97,98,99,100,101,102]"
    );
    // Scores are u16s to the client; s16 reads them signed.
    assert_eq!(
        c.handle("3:read u16 0x8040004c 4").unwrap(),
        "3:[65534,300]"
    );
    assert_eq!(c.handle("4:read s16 0x8040004c 4").unwrap(), "4:[-2,300]");
    // KSEG1 and physical addresses are the same RDRAM.
    assert_eq!(
        c.handle("5:read u32 0xa0400000 4").unwrap(),
        "5:[1330331993]"
    );
    assert_eq!(c.handle("6:read u32 0x400000 4").unwrap(), "6:[1330331993]");
    // A size that is not a whole number of values reads one more, as upstream steps.
    assert_eq!(
        c.handle("7:read u32 0x80400000 6").unwrap(),
        "7:[1330331993,0]"
    );
    assert_eq!(
        c.handle("8:read bytestring 0x80400000 4").unwrap(),
        "8:OKAY"
    );
}

#[test]
fn writes_what_the_client_sends() {
    let (c, ram) = connector();
    // receive_snap_items: a mask, and json.dumps' spacing in a list of several.
    assert_eq!(
        c.handle("9:write u32 0x8040000c [4294967295]").unwrap(),
        "9:Write successful"
    );
    assert_eq!(
        c.handle("10:write u32 0x80400010 [1, 2, 3, 4, 5, 6, 7]")
            .unwrap(),
        "10:Write successful"
    );
    assert_eq!(
        c.handle("11:write s16 0x80400100 [-1, 2]").unwrap(),
        "11:Write successful"
    );
    let ram = ram.borrow();
    assert_eq!(&ram.ram[0x40_000C..0x40_0010], &[0xFF; 4]);
    assert_eq!(&ram.ram[0x40_0010..0x40_0018], &[0, 0, 0, 1, 0, 0, 0, 2]);
    assert_eq!(&ram.ram[0x40_0028..0x40_002C], &[0, 0, 0, 7]);
    assert_eq!(&ram.ram[0x40_0100..0x40_0104], &[0xFF, 0xFF, 0, 2]);
}

#[test]
fn answers_rom_info_and_dict_as_upstream() {
    let (c, _) = connector();
    let info = c.handle("12:romInfo").unwrap();
    let json: serde_json::Value = serde_json::from_str(info.strip_prefix("12:").unwrap()).unwrap();
    assert_eq!(json["name"], "POKEMON SNAP");
    assert_eq!(json["id"], "NPFE");
    assert_eq!(json["crc1"], 0xCA12_B547u32);
    assert_eq!(json["crc2"], 0x71FA_4EE4u32);

    let dict = c
        .handle(r#"13:dict {"magic": {"adr": 2151677952, "type": "u32", "size": 4}, "first": 2151677952}"#)
        .unwrap();
    let json: serde_json::Value = serde_json::from_str(dict.strip_prefix("13:").unwrap()).unwrap();
    assert_eq!(json["magic"], serde_json::json!([1330331993]));
    assert_eq!(json["first"], serde_json::json!([79]));
}

#[test]
fn a_bad_request_is_answered_as_upstream_answers_it() {
    let (c, _) = connector();
    // The client raises each of these as a script error, from its first word.
    assert_eq!(
        c.handle("14:read u32 nope 4").unwrap(),
        "14:Invalid read parameters"
    );
    assert_eq!(
        c.handle("15:read u32 0x80400000 0").unwrap(),
        "15:Invalid read parameters"
    );
    assert_eq!(
        c.handle("16:write u32 0x80400000 7").unwrap(),
        "16:Value must be a valid JSON Array"
    );
    assert_eq!(c.handle("17:frobnicate").unwrap(), "17:Unknown command");
    assert_eq!(c.handle("no id").unwrap(), "ERR:Invalid message format");
    // Outside RDRAM fails only that line: the client is reset and connects again.
    assert!(c.handle("18:read u32 0x80800000 4").is_err());
}

/// A `bytestring` is one character per byte both ways, as upstream's `String.fromCharCode` and
/// `charCodeAt` make it, and the client's UTF-8 carries it: a byte past 0x7F is the character
/// with that code, not a byte that is not UTF-8.
#[test]
fn a_bytestring_is_one_character_per_byte() {
    let (c, ram) = connector();
    // FF FE 01 2C at 0x8040004C.
    assert_eq!(
        c.handle("1:read bytestring 0x8040004c 4").unwrap(),
        "1:\u{FF}\u{FE}\u{1},"
    );
    // One byte a character, each its code's low byte; a character past U+FFFF is two UTF-16
    // units, so two bytes, as charCodeAt sees it.
    assert_eq!(
        c.handle("2:write bytestring 0x80400200 aé🎮").unwrap(),
        "2:Write successful"
    );
    assert_eq!(
        &ram.borrow().ram[0x40_0200..0x40_0205],
        &[0x61, 0xE9, 0x3C, 0xAE, 0]
    );
}

/// Requests Pokémon Snap Client 0.8.0 never makes are still answered as upstream's script
/// answers them, so a client that starts making them is not reset for it.
#[test]
fn an_odd_request_is_answered_as_upstream_answers_it() {
    let (c, ram) = connector();
    // A JSON object where a write wants a list.
    assert_eq!(
        c.handle(r#"1:write u32 0x80400000 {"a": 1}"#).unwrap(),
        "1:Value must be a valid JSON Array"
    );
    // A dict entry's size: 0 is upstream's `|| 1`, one value; a negative one reads none; one
    // that is not whole steps as upstream's loop does, 2.5 bytes of u8 being three.
    let dict = c
        .handle(r#"2:dict {"one": {"adr": 2151677952, "size": 0}, "none": {"adr": 2151677952, "size": -1}, "three": {"adr": 2151677952, "size": 2.5}}"#)
        .unwrap();
    let json: serde_json::Value = serde_json::from_str(dict.strip_prefix("2:").unwrap()).unwrap();
    assert_eq!(json["one"], serde_json::json!([79]));
    assert_eq!(json["none"], serde_json::json!([]));
    assert_eq!(json["three"], serde_json::json!([79, 75, 65]));
    // An address that is not whole is a bad request, not a reset.
    assert_eq!(
        c.handle(r#"3:dict {"x": {"adr": 2151677952.5}}"#).unwrap(),
        "3:Invalid Dict JSON Format"
    );
    // parseInt takes the leading digits and leaves the rest.
    assert_eq!(
        c.handle("4:read u8 0x80400000zz 2abc").unwrap(),
        "4:[79,75]"
    );
    // Values are converted as JavaScript stores a number in a byte: truncated toward zero,
    // wrapped, and null, a numeric string or a boolean taken as the number it is.
    assert_eq!(
        c.handle(r#"5:write u8 0x80400300 [1.9, -1.5, 256, null, "7", true]"#)
            .unwrap(),
        "5:Write successful"
    );
    assert_eq!(
        &ram.borrow().ram[0x40_0300..0x40_0306],
        &[1, 0xFF, 0, 0, 7, 1]
    );
}
