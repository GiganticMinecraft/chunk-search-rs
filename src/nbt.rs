//! チャンクの判定に必要な範囲だけを読む NBT デコーダ。
//!
//! named-binary-tag 0.6 (`nbt::decode`) のデコード処理と同じ結果になるように書いている。
//! 互換性のため、以下の点は意図的に named-binary-tag と揃えている。
//!
//! - 文字列は UTF-8 として lossy に読む (不正なバイト列や対になっていないサロゲートでも失敗しない)
//! - 配列・リストの長さは u32 として読む
//! - コンパウンド内でキーが重複した場合は後の値で上書きする
//! - ルートタグの後ろに続くデータは読まない (zlib/gzip のストリームも末尾まで読まない)
//! - エラーの種類と、region.rs が出す警告の文言。ただし `DecodeError::Io` の Display は
//!   named-binary-tag の "IO Error" ではなく中の io::Error を表示する
//!   (従来の region.rs も io::Error を取り出して表示していたので、警告の文言は変わらない)
//!
//! named-binary-tag と違い、再帰を使わずに読むので入れ子が深くてもスタックを溢れさせない。
//! named-binary-tag は深さ約 3,600 を超えるとワーカースレッドのスタック (2 MiB) を溢れさせ、
//! プロセスごと異常終了していた。代わりに深さ [`MAX_DEPTH`] を超えたら `DecodeError` を返す。
//!
//! また判定に使わない値は保持しない。リストは要素数だけを残してリスト配下のタグは読み捨て、
//! 文字列・バイト配列・long 配列は種別だけを残す。

use flate2::read::{GzDecoder, ZlibDecoder};
use std::collections::HashMap;
use std::fmt;
use std::io::{self, Read};

/// コンパウンドタグの中身。
pub type Compound = HashMap<String, Tag>;

/// デコード結果のタグ。判定に使わない値は中身を持たない。
#[derive(Debug, Clone, PartialEq)]
pub enum Tag {
    Byte,
    Short,
    Int(i32),
    Long,
    Float,
    Double,
    ByteArray,
    String,
    /// 要素は保持せず、要素数だけを持つ。
    List(u32),
    Compound(Compound),
    IntArray(Vec<i32>),
    LongArray,
}

/// 深く入れ子になったコンパウンドを再帰せずに解放する。
///
/// 既定の Drop は入れ子の深さだけ再帰するので、深いコンパウンドの解放でスタックが溢れる。
impl Drop for Tag {
    fn drop(&mut self) {
        let Tag::Compound(tags) = self else {
            return;
        };
        let mut pending = vec![std::mem::take(tags)];
        while let Some(mut tags) = pending.pop() {
            for (_, mut child) in tags.drain() {
                if let Tag::Compound(grandchildren) = &mut child {
                    pending.push(std::mem::take(grandchildren));
                }
                // child の中身は空にしてあるので、ここでの解放は再帰しない
            }
        }
    }
}

impl Tag {
    /// named-binary-tag と同じタグ種別名。
    pub fn type_name(&self) -> &'static str {
        match self {
            Tag::Byte => "TAG_Byte",
            Tag::Short => "TAG_Short",
            Tag::Int(_) => "TAG_Int",
            Tag::Long => "TAG_Long",
            Tag::Float => "TAG_Float",
            Tag::Double => "TAG_Double",
            Tag::ByteArray => "TAG_Byte_Array",
            Tag::String => "TAG_String",
            Tag::List(_) => "TAG_List",
            Tag::Compound(_) => "TAG_Compound",
            Tag::IntArray(_) => "TAG_Int_Array",
            Tag::LongArray => "TAG_Long_Array",
        }
    }
}

/// 読み途中のリスト・コンパウンドを何段まで重ねられるか。
///
/// named-binary-tag は深さ約 3,600 で必ずプロセスごと落ちていたので、それより十分大きい値にしている。
/// これを超える入力は従来も結果を出せなかったので、警告にしても読めていたものは減らない。
/// 読み途中の状態に使うメモリはこの深さに比例する (1 段あたり 100 byte 未満)。
pub const MAX_DEPTH: usize = 100_000;

/// named-binary-tag の `TagDecodeError` と同じ分類のエラー (`DepthLimitExceeded` を除く)。
#[derive(Debug)]
pub enum DecodeError {
    RootMustBeCompoundTag { actual_type: &'static str },
    UnknownTagType { tag_type_id: u8 },
    DepthLimitExceeded,
    Io(io::Error),
}

impl From<io::Error> for DecodeError {
    fn from(e: io::Error) -> Self {
        DecodeError::Io(e)
    }
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DecodeError::RootMustBeCompoundTag { actual_type } => {
                write!(f, "Root must be a TAG_Compound but is a {actual_type}")
            }
            DecodeError::UnknownTagType { tag_type_id } => {
                write!(f, "Unknown tag type: {tag_type_id}")
            }
            DecodeError::DepthLimitExceeded => {
                write!(f, "nesting is deeper than {MAX_DEPTH} levels")
            }
            DecodeError::Io(e) => write!(f, "{e}"),
        }
    }
}

pub fn read_gzip_compound_tag(data: &[u8]) -> Result<Compound, DecodeError> {
    read_compound_tag(&mut GzDecoder::new(data))
}

pub fn read_zlib_compound_tag(data: &[u8]) -> Result<Compound, DecodeError> {
    read_compound_tag(&mut ZlibDecoder::new(data))
}

/// ルートのコンパウンドタグを読む。
pub fn read_compound_tag<R: Read>(reader: &mut R) -> Result<Compound, DecodeError> {
    let tag_id = read_u8(reader)?;
    skip_string(reader)?;
    match &mut read_tag(tag_id, reader)? {
        // Tag は Drop を実装しているので中身は take で取り出す
        Tag::Compound(tags) => Ok(std::mem::take(tags)),
        other => Err(DecodeError::RootMustBeCompoundTag {
            actual_type: other.type_name(),
        }),
    }
}

fn read_array<const N: usize, R: Read>(reader: &mut R) -> io::Result<[u8; N]> {
    let mut buf = [0u8; N];
    reader.read_exact(&mut buf)?;
    Ok(buf)
}

fn read_u8<R: Read>(reader: &mut R) -> io::Result<u8> {
    Ok(read_array::<1, _>(reader)?[0])
}

fn read_u16<R: Read>(reader: &mut R) -> io::Result<u16> {
    Ok(u16::from_be_bytes(read_array(reader)?))
}

fn read_u32<R: Read>(reader: &mut R) -> io::Result<u32> {
    Ok(u32::from_be_bytes(read_array(reader)?))
}

fn read_i32<R: Read>(reader: &mut R) -> io::Result<i32> {
    Ok(i32::from_be_bytes(read_array(reader)?))
}

/// `len` バイトを読み捨てる。足りなければ `read_exact` と同じ UnexpectedEof を返す。
fn skip<R: Read>(reader: &mut R, len: u64) -> io::Result<()> {
    let copied = io::copy(&mut reader.by_ref().take(len), &mut io::sink())?;
    if copied < len {
        return Err(unexpected_eof());
    }
    Ok(())
}

fn unexpected_eof() -> io::Error {
    io::Error::new(io::ErrorKind::UnexpectedEof, "failed to fill whole buffer")
}

fn skip_string<R: Read>(reader: &mut R) -> io::Result<()> {
    let len = read_u16(reader)?;
    skip(reader, len.into())
}

fn read_name<R: Read>(reader: &mut R) -> io::Result<String> {
    let len = read_u16(reader)?;
    let mut buf = vec![0; len as usize];
    reader.read_exact(&mut buf)?;
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

/// int 配列の本体を読む。長さが壊れていても巨大な確保をしないよう、実際に読めた分だけ確保する。
fn read_int_array<R: Read>(reader: &mut R, len: u32) -> io::Result<Vec<i32>> {
    let byte_len = u64::from(len) * 4;
    let mut buf = Vec::new();
    reader.by_ref().take(byte_len).read_to_end(&mut buf)?;
    if (buf.len() as u64) < byte_len {
        return Err(unexpected_eof());
    }
    Ok(buf
        .as_chunks::<4>()
        .0
        .iter()
        .map(|c| i32::from_be_bytes(*c))
        .collect())
}

/// 子を持たないタグ (リスト・コンパウンド以外) を読む。
///
/// `keep` が false のときは値を作らずに読み捨てて `None` を返す。
/// 呼び出し側で種別が既知 (1..=8, 11, 12) であることを確認しておくこと。
fn read_leaf<R: Read>(tag_id: u8, keep: bool, reader: &mut R) -> io::Result<Option<Tag>> {
    let tag = match tag_id {
        1 => read_array::<1, _>(reader).map(|_| Tag::Byte)?,
        2 => read_array::<2, _>(reader).map(|_| Tag::Short)?,
        3 => Tag::Int(read_i32(reader)?),
        4 => read_array::<8, _>(reader).map(|_| Tag::Long)?,
        5 => read_array::<4, _>(reader).map(|_| Tag::Float)?,
        6 => read_array::<8, _>(reader).map(|_| Tag::Double)?,
        7 => {
            let len = read_u32(reader)?;
            skip(reader, u64::from(len)).map(|_| Tag::ByteArray)?
        }
        8 => skip_string(reader).map(|_| Tag::String)?,
        11 => {
            let len = read_u32(reader)?;
            if !keep {
                skip(reader, u64::from(len) * 4)?;
                return Ok(None);
            }
            Tag::IntArray(read_int_array(reader, len)?)
        }
        12 => {
            let len = read_u32(reader)?;
            skip(reader, u64::from(len) * 8).map(|_| Tag::LongArray)?
        }
        _ => unreachable!("unknown tag types are rejected by the caller"),
    };
    Ok(keep.then_some(tag))
}

/// 読みかけのリスト・コンパウンド。
enum Frame {
    /// `keep` が false なら中身は保持しない。`key` は読み途中の値のキー。
    Compound {
        tags: Compound,
        keep: bool,
        key: Option<String>,
    },
    List {
        element_id: u8,
        remaining: u32,
        len: u32,
        keep: bool,
    },
}

/// `tag_id` のタグの本体を読む。入れ子は再帰ではなく `stack` で追う。
fn read_tag<R: Read>(tag_id: u8, reader: &mut R) -> Result<Tag, DecodeError> {
    let mut stack: Vec<Frame> = Vec::new();
    // これから読むタグの種別と、読んだ値を保持するか
    let mut next = Some((tag_id, true));

    loop {
        // 読み終えた値。外側の Option は「読み終えたか」、内側は「値を保持するか」
        let mut finished: Option<Option<Tag>> = None;
        if let Some((id, keep)) = next.take() {
            match id {
                9 | 10 if stack.len() >= MAX_DEPTH => return Err(DecodeError::DepthLimitExceeded),
                9 => {
                    let element_id = read_u8(reader)?;
                    let len = read_u32(reader)?;
                    stack.push(Frame::List {
                        element_id,
                        remaining: len,
                        len,
                        keep,
                    });
                }
                10 => stack.push(Frame::Compound {
                    tags: Compound::new(),
                    keep,
                    key: None,
                }),
                1..=8 | 11 | 12 => finished = Some(read_leaf(id, keep, reader)?),
                id => return Err(DecodeError::UnknownTagType { tag_type_id: id }),
            }
        }

        // 読み終えた値を親に渡し、次に読むタグを決める
        loop {
            if let Some(value) = finished.take() {
                match stack.last_mut() {
                    None => return Ok(value.expect("the root tag is always kept")),
                    Some(Frame::Compound { tags, key, .. }) => {
                        let key = key.take().expect("key is read before its value");
                        if let Some(value) = value {
                            tags.insert(key, value);
                        }
                    }
                    Some(Frame::List { .. }) => {}
                }
            }

            match stack.last_mut().expect("stack is not empty here") {
                Frame::Compound { keep, key, .. } => {
                    let id = read_u8(reader)?;
                    if id == 0 {
                        let Some(Frame::Compound { tags, keep, .. }) = stack.pop() else {
                            unreachable!()
                        };
                        finished = Some(keep.then_some(Tag::Compound(tags)));
                        continue;
                    }
                    *key = Some(if *keep {
                        read_name(reader)?
                    } else {
                        skip_string(reader)?;
                        String::new()
                    });
                    next = Some((id, *keep));
                    break;
                }
                Frame::List {
                    element_id,
                    remaining,
                    len,
                    keep,
                } => {
                    if *remaining == 0 {
                        finished = Some(keep.then_some(Tag::List(*len)));
                        stack.pop();
                        continue;
                    }
                    *remaining -= 1;
                    next = Some((*element_id, false));
                    break;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn named(tag_id: u8, name: &[u8], payload: &[u8]) -> Vec<u8> {
        let mut v = vec![tag_id];
        v.extend((name.len() as u16).to_be_bytes());
        v.extend(name);
        v.extend(payload);
        v
    }

    fn decode(bytes: &[u8]) -> Result<Compound, DecodeError> {
        read_compound_tag(&mut &bytes[..])
    }

    #[test]
    fn reads_values_needed_for_detection() {
        let mut payload = named(3, b"xPos", &(-3i32).to_be_bytes());
        payload.extend(named(
            11,
            b"Position",
            &[0, 0, 0, 2, 0, 0, 0, 1, 255, 255, 255, 254],
        ));
        payload.extend(named(9, b"Entities", &[10, 0, 0, 0, 1, 0]));
        payload.push(0);
        let root = decode(&named(10, b"", &payload)).unwrap();
        assert_eq!(root.get("xPos"), Some(&Tag::Int(-3)));
        assert_eq!(root.get("Position"), Some(&Tag::IntArray(vec![1, -2])));
        assert_eq!(root.get("Entities"), Some(&Tag::List(1)));
    }

    #[test]
    fn strings_are_read_lossily() {
        // 対になっていないサロゲート (CESU-8) と不正な UTF-8 をキーと値に含む
        let mut payload = named(8, b"\xed\xa0\x80", &[0, 2, 0xff, 0xfe]);
        payload.extend(named(3, b"xPos", &1i32.to_be_bytes()));
        payload.push(0);
        let root = decode(&named(10, b"", &payload)).unwrap();
        assert_eq!(root.get("xPos"), Some(&Tag::Int(1)));
    }

    #[test]
    fn duplicate_keys_keep_the_last_value() {
        let mut payload = named(3, b"xPos", &1i32.to_be_bytes());
        payload.extend(named(3, b"xPos", &2i32.to_be_bytes()));
        payload.push(0);
        let root = decode(&named(10, b"", &payload)).unwrap();
        assert_eq!(root.get("xPos"), Some(&Tag::Int(2)));
    }

    #[test]
    fn trailing_data_is_ignored() {
        let mut bytes = named(10, b"", &[0]);
        bytes.extend(b"garbage");
        assert!(decode(&bytes).is_ok());
    }

    #[test]
    fn deep_nesting_does_not_overflow_the_stack() {
        // named-binary-tag は深さ約 3,600 で落ちていた。
        // ルートのコンパウンドと "deep" のリストを合わせて、ちょうど MAX_DEPTH 段にする
        let depth = MAX_DEPTH - 3;
        let mut deep = Vec::new();
        for _ in 0..depth {
            deep.push(9);
            deep.extend(1u32.to_be_bytes());
        }
        // 最も内側は要素型 Byte の空リスト
        deep.push(1);
        deep.extend(0u32.to_be_bytes());
        let mut body = named(9, b"deep", &deep);
        body.extend(named(3, b"xPos", &7i32.to_be_bytes()));
        body.push(0);
        let root = decode(&named(10, b"", &body)).unwrap();
        assert_eq!(root.get("deep"), Some(&Tag::List(1)));
        assert_eq!(root.get("xPos"), Some(&Tag::Int(7)));
    }

    #[test]
    fn deep_compounds_are_built_and_dropped_without_overflow() {
        // {d:{d:{...}}} を MAX_DEPTH 段 (ルートを含む) 重ねる。値を保持するので木ができる
        let depth = MAX_DEPTH - 1;
        let mut body = Vec::new();
        for _ in 0..depth {
            body.push(10);
            body.extend(1u16.to_be_bytes());
            body.push(b'd');
        }
        body.extend(std::iter::repeat_n(0, depth + 1));
        let root = decode(&named(10, b"", &body)).unwrap();
        assert!(matches!(root.get("d"), Some(Tag::Compound(_))));
        drop(root);
    }

    #[test]
    fn too_deep_nesting_is_an_error() {
        let mut deep = Vec::new();
        for _ in 0..MAX_DEPTH {
            deep.push(9);
            deep.extend(1u32.to_be_bytes());
        }
        deep.push(1);
        deep.extend(0u32.to_be_bytes());
        let mut body = named(9, b"deep", &deep);
        body.push(0);
        assert_eq!(
            decode(&named(10, b"", &body)).unwrap_err().to_string(),
            format!("nesting is deeper than {MAX_DEPTH} levels")
        );
    }

    #[test]
    fn errors_match_named_binary_tag() {
        assert_eq!(
            decode(&named(3, b"", &1i32.to_be_bytes()))
                .unwrap_err()
                .to_string(),
            "Root must be a TAG_Compound but is a TAG_Int"
        );
        assert_eq!(
            decode(&named(10, b"", &named(13, b"x", &[])))
                .unwrap_err()
                .to_string(),
            "Unknown tag type: 13"
        );
        assert_eq!(
            decode(&named(10, b"", &[3, 0])).unwrap_err().to_string(),
            "failed to fill whole buffer"
        );
        // 要素型 End のリストは、要素が無ければ読めるが要素があれば失敗する
        let mut empty = named(9, b"l", &[0, 0, 0, 0, 0]);
        empty.push(0);
        assert!(decode(&named(10, b"", &empty)).is_ok());
        let mut non_empty = named(9, b"l", &[0, 0, 0, 0, 1]);
        non_empty.push(0);
        assert_eq!(
            decode(&named(10, b"", &non_empty)).unwrap_err().to_string(),
            "Unknown tag type: 0"
        );
    }
}
