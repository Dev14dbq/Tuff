//! Vanilla's size limits on what a client sends.

use mc_protocol::Reader;
use mc_protocol::Write as _;
use mc_protocol::packets::{MAX_KNOWN_PACKS, MAX_PAYLOAD_LEN, MAX_QUERY_ANSWER_LEN};
use mc_protocol::packets::{configuration, login, play};

fn payload(len: usize) -> Vec<u8> {
    let mut body = Vec::new();
    body.put_string("minecraft:brand");
    body.resize(body.len() + len, b'x');
    body
}

#[test]
fn custom_payload_is_at_most_32767_bytes() {
    for (len, ok) in [(MAX_PAYLOAD_LEN, true), (MAX_PAYLOAD_LEN + 1, false)] {
        let body = payload(len);
        let config =
            configuration::Serverbound::decode("minecraft:custom_payload", &mut Reader::new(&body));
        assert_eq!(config.is_ok(), ok, "configuration, {len} bytes");
        let game = play::Serverbound::decode("minecraft:custom_payload", &mut Reader::new(&body));
        assert_eq!(game.is_ok(), ok, "play, {len} bytes");
    }
}

#[test]
fn login_query_answer_is_at_most_a_mebibyte() {
    for (len, ok) in [
        (MAX_QUERY_ANSWER_LEN, true),
        (MAX_QUERY_ANSWER_LEN + 1, false),
    ] {
        let mut body = Vec::new();
        body.put_varint(1);
        body.put_bool(true);
        body.resize(body.len() + len, 0);
        let answer =
            login::Serverbound::decode("minecraft:custom_query_answer", &mut Reader::new(&body));
        assert_eq!(answer.is_ok(), ok, "{len} bytes");
    }
}

#[test]
fn known_packs_list_is_at_most_64() {
    for (count, ok) in [(MAX_KNOWN_PACKS, true), (MAX_KNOWN_PACKS + 1, false)] {
        let mut body = Vec::new();
        body.put_len(count);
        for _ in 0..count {
            body.put_string("minecraft");
            body.put_string("core");
            body.put_string("26.3");
        }
        let packs = configuration::Serverbound::decode(
            "minecraft:select_known_packs",
            &mut Reader::new(&body),
        );
        assert_eq!(packs.is_ok(), ok, "{count} packs");
    }
}

#[test]
fn strings_keep_vanilla_lengths() {
    // Chat is 256 characters, a sign line 384, a book page 1024.
    let mut chat = Vec::new();
    chat.put_string(&"a".repeat(257));
    assert!(play::Serverbound::decode("minecraft:chat", &mut Reader::new(&chat)).is_err());

    let mut sign = Vec::new();
    sign.put_i64(0);
    sign.put_string(&"a".repeat(385));
    assert!(play::Serverbound::decode("minecraft:sign_update", &mut Reader::new(&sign)).is_err());

    let book = |pages: usize, page_len: usize| {
        let mut body = Vec::new();
        body.put_varint(0);
        body.put_len(pages);
        for _ in 0..pages {
            body.put_string(&"a".repeat(page_len));
        }
        body.put_bool(false);
        play::Serverbound::decode("minecraft:edit_book", &mut Reader::new(&body)).is_ok()
    };
    assert!(book(100, 1024));
    assert!(!book(101, 1));
    assert!(!book(1, 1025));
}

#[test]
fn packet_nbt_has_the_network_quota() {
    // A compound holding one byte array: 48 + 28 + 2 + 36 + 24 quota bytes
    // around the array itself, which may take the rest of the 2 MiB.
    let nbt = |len: usize| {
        let mut nbt = vec![10u8, 7, 0, 1, b'a'];
        nbt.extend_from_slice(&(len as i32).to_be_bytes());
        nbt.resize(nbt.len() + len, 0);
        nbt.push(0);
        nbt
    };
    let fits = (2 << 20) - (48 + 28 + 2 + 36 + 24);
    assert!(Reader::new(&nbt(fits)).nbt().is_ok());
    assert!(Reader::new(&nbt(fits + 1)).nbt().is_err());
    assert!(Reader::new(&nbt(fits + 1)).nbt_optional().is_err());
}
