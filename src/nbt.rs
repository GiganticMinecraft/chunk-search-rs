//! チャンクの判定に必要な範囲だけを読む NBT デコーダ。
//!
//! named-binary-tag 0.6 (`nbt::decode`) のデコード処理と同じ挙動になるように書いている。
//! 互換性のため、以下の点は意図的に named-binary-tag と揃えている。
//!
//! - 文字列は UTF-8 として lossy に読む (不正なバイト列や対になっていないサロゲートでも失敗しない)
//! - 配列・リストの長さは u32 として読む
//! - コンパウンド内でキーが重複した場合は後の値で上書きする
//! - ルートタグの後ろに続くデータは読まない (zlib/gzip のストリームも末尾まで読まない)
//! - 読み出しは型ごとに必要なバイト数ずつ行う
//!
//! 判定に使わない文字列・バイト配列・long 配列の中身は保持せずに読み捨てる。

use flate2::read::{GzDecoder, ZlibDecoder};
use std::collections::HashMap;
use std::fmt;
use std::io::{self, Read};

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
    List(Vec<Tag>),
    Compound(HashMap<String, Tag>),
    IntArray(Vec<i32>),
    LongArray,
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

    /// コンパウンドなら `name` の値を返す。
    pub fn get(&self, name: &str) -> Option<&Tag> {
        match self {
            Tag::Compound(tags) => tags.get(name),
            _ => None,
        }
    }
}

/// named-binary-tag の `TagDecodeError` と同じ分類・表示のエラー。
#[derive(Debug)]
pub enum DecodeError {
    RootMustBeCompoundTag { actual_type: &'static str },
    UnknownTagType { tag_type_id: u8 },
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
            DecodeError::Io(e) => write!(f, "{e}"),
        }
    }
}

pub fn read_gzip_compound_tag(data: &[u8]) -> Result<Tag, DecodeError> {
    read_compound_tag(&mut GzDecoder::new(data))
}

pub fn read_zlib_compound_tag(data: &[u8]) -> Result<Tag, DecodeError> {
    read_compound_tag(&mut ZlibDecoder::new(data))
}

/// ルートのコンパウンドタグを読む。
pub fn read_compound_tag<R: Read>(reader: &mut R) -> Result<Tag, DecodeError> {
    let tag_id = read_u8(reader)?;
    skip_string(reader)?;
    match read_tag(tag_id, reader)? {
        tag @ Tag::Compound(_) => Ok(tag),
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

fn read_u32<R: Read>(reader: &mut R) -> io::Result<u32> {
    Ok(u32::from_be_bytes(read_array(reader)?))
}

fn read_i32<R: Read>(reader: &mut R) -> io::Result<i32> {
    Ok(i32::from_be_bytes(read_array(reader)?))
}

fn read_string_bytes<R: Read>(reader: &mut R) -> io::Result<Vec<u8>> {
    let length = u16::from_be_bytes(read_array(reader)?);
    let mut buf = vec![0; length as usize];
    reader.read_exact(&mut buf)?;
    Ok(buf)
}

fn skip_string<R: Read>(reader: &mut R) -> io::Result<()> {
    read_string_bytes(reader).map(|_| ())
}

fn read_tag<R: Read>(tag_id: u8, reader: &mut R) -> Result<Tag, DecodeError> {
    let tag = match tag_id {
        1 => {
            read_array::<1, _>(reader)?;
            Tag::Byte
        }
        2 => {
            read_array::<2, _>(reader)?;
            Tag::Short
        }
        3 => Tag::Int(read_i32(reader)?),
        4 => {
            read_array::<8, _>(reader)?;
            Tag::Long
        }
        5 => {
            read_array::<4, _>(reader)?;
            Tag::Float
        }
        6 => {
            read_array::<8, _>(reader)?;
            Tag::Double
        }
        7 => {
            let length = read_u32(reader)?;
            for _ in 0..length {
                read_array::<1, _>(reader)?;
            }
            Tag::ByteArray
        }
        8 => {
            skip_string(reader)?;
            Tag::String
        }
        9 => {
            let list_tags_id = read_u8(reader)?;
            let length = read_u32(reader)?;
            let mut value = Vec::new();
            for _ in 0..length {
                value.push(read_tag(list_tags_id, reader)?);
            }
            Tag::List(value)
        }
        10 => {
            let mut tags = HashMap::new();
            loop {
                let tag_id = read_u8(reader)?;
                if tag_id == 0 {
                    break;
                }
                let name = String::from_utf8_lossy(&read_string_bytes(reader)?).into_owned();
                let tag = read_tag(tag_id, reader)?;
                tags.insert(name, tag);
            }
            Tag::Compound(tags)
        }
        11 => {
            let length = read_u32(reader)?;
            let mut value = Vec::new();
            for _ in 0..length {
                value.push(read_i32(reader)?);
            }
            Tag::IntArray(value)
        }
        12 => {
            let length = read_u32(reader)?;
            for _ in 0..length {
                read_array::<8, _>(reader)?;
            }
            Tag::LongArray
        }
        tag_type_id => return Err(DecodeError::UnknownTagType { tag_type_id }),
    };
    Ok(tag)
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

    fn decode(bytes: &[u8]) -> Result<Tag, DecodeError> {
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
        assert_eq!(
            root.get("Entities"),
            Some(&Tag::List(vec![Tag::Compound(HashMap::new())]))
        );
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
