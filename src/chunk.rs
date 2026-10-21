//! チャンク NBT からエンティティ / ブロックエンティティの有無を判定する。

use crate::nbt::Tag;

/// 21w43a (1.18 開発版)。これ以降のチャンクは `Level` が無くなり、フィールドがルート直下に置かれる。
pub const DATA_VERSION_21W43A: i32 = 2844;

// エラーメッセージは named-binary-tag 0.6 の `CompoundTagError` と同じ表記にしている。
fn not_found(name: &str) -> String {
    format!("Tag {name} not found")
}

fn wrong_type(name: &str, actual: &Tag) -> String {
    format!("Tag {name} has type {}", actual.type_name())
}

fn required<'a>(tag: &'a Tag, name: &str) -> Result<&'a Tag, String> {
    tag.get(name).ok_or_else(|| not_found(name))
}

fn as_i32(value: &Tag, name: &str) -> Result<i32, String> {
    match value {
        Tag::Int(v) => Ok(*v),
        other => Err(wrong_type(name, other)),
    }
}

fn get_i32(tag: &Tag, name: &str) -> Result<i32, String> {
    as_i32(required(tag, name)?, name)
}

fn get_compound<'a>(tag: &'a Tag, name: &str) -> Result<&'a Tag, String> {
    match required(tag, name)? {
        v @ Tag::Compound(_) => Ok(v),
        other => Err(wrong_type(name, other)),
    }
}

/// リスト型タグが存在して 1 要素以上あるか。存在しなければ false。
fn has_entries(tag: &Tag, name: &str) -> Result<bool, String> {
    match tag.get(name) {
        None => Ok(false),
        Some(Tag::List(v)) => Ok(!v.is_empty()),
        Some(other) => Err(wrong_type(name, other)),
    }
}

/// `names` のいずれかのリストに要素があれば true。
/// 一部のリストが壊れていても、他のリストに要素があれば true を返す。
fn any_has_entries(tag: &Tag, names: &[&str]) -> Result<bool, String> {
    let mut first_error = None;
    for name in names {
        match has_entries(tag, name) {
            Ok(true) => return Ok(true),
            Ok(false) => {}
            Err(e) => {
                first_error.get_or_insert(e);
            }
        }
    }
    first_error.map_or(Ok(false), Err)
}

/// `region/` のチャンク NBT を調べ、エンティティかブロックエンティティを含むならチャンク座標を返す。
///
/// - DataVersion >= 2844 (1.18 形式): ルート直下の `xPos`/`zPos`/`block_entities`
///   (未完成チャンクの `entities` も見る)
/// - それ未満または DataVersion 無し (1.17 以前): `Level` 配下の `xPos`/`zPos`/`Entities`/`TileEntities`。
///   1.17 では `Entities` が `entities/` に移ったので無くてもよい。
pub fn region_chunk_with_entities(chunk: &Tag) -> Result<Option<(i32, i32)>, String> {
    let data_version = chunk
        .get("DataVersion")
        .map(|v| as_i32(v, "DataVersion"))
        .transpose()?;

    let (base, entity_lists): (&Tag, &[&str]) = match data_version {
        Some(v) if v >= DATA_VERSION_21W43A => (chunk, &["block_entities", "entities"]),
        _ => (get_compound(chunk, "Level")?, &["TileEntities", "Entities"]),
    };

    let x = get_i32(base, "xPos")?;
    let z = get_i32(base, "zPos")?;

    Ok(any_has_entries(base, entity_lists)?.then_some((x, z)))
}

/// `entities/` (1.17 以降) のチャンク NBT を調べ、エンティティを含むならチャンク座標を返す。
pub fn entity_chunk_with_entities(chunk: &Tag) -> Result<Option<(i32, i32)>, String> {
    if !has_entries(chunk, "Entities")? {
        return Ok(None);
    }
    let position = match required(chunk, "Position")? {
        Tag::IntArray(position) => position,
        other => return Err(wrong_type("Position", other)),
    };
    match position.as_slice() {
        [x, z] => Ok(Some((*x, *z))),
        other => Err(format!(
            "Position must have 2 elements but has {}",
            other.len()
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn compound<const N: usize>(entries: [(&str, Tag); N]) -> Tag {
        Tag::Compound(
            entries
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect(),
        )
    }

    fn set(tag: &mut Tag, name: &str, value: Tag) {
        let Tag::Compound(map) = tag else {
            panic!("not a compound")
        };
        map.insert(name.to_string(), value);
    }

    fn level_mut(tag: &mut Tag) -> &mut Tag {
        let Tag::Compound(map) = tag else {
            panic!("not a compound")
        };
        map.get_mut("Level").unwrap()
    }

    fn chest() -> Tag {
        compound([("id", Tag::String)])
    }

    fn chunk_118(be: Vec<Tag>) -> Tag {
        compound([
            ("DataVersion", Tag::Int(2975)),
            ("xPos", Tag::Int(3)),
            ("zPos", Tag::Int(-4)),
            ("block_entities", Tag::List(be)),
        ])
    }

    fn chunk_legacy(data_version: Option<i32>, entities: Option<Vec<Tag>>, te: Vec<Tag>) -> Tag {
        let mut level = compound([
            ("xPos", Tag::Int(-1)),
            ("zPos", Tag::Int(2)),
            ("TileEntities", Tag::List(te)),
        ]);
        if let Some(e) = entities {
            set(&mut level, "Entities", Tag::List(e));
        }
        let mut c = compound([("Level", level)]);
        if let Some(v) = data_version {
            set(&mut c, "DataVersion", Tag::Int(v));
        }
        c
    }

    #[test]
    fn reads_118_block_entities() {
        assert_eq!(
            region_chunk_with_entities(&chunk_118(vec![chest()])),
            Ok(Some((3, -4)))
        );
        assert_eq!(region_chunk_with_entities(&chunk_118(vec![])), Ok(None));
    }

    #[test]
    fn reads_legacy_formats() {
        // 1.12 (DataVersion 無し)
        assert_eq!(
            region_chunk_with_entities(&chunk_legacy(None, Some(vec![]), vec![chest()])),
            Ok(Some((-1, 2)))
        );
        // 1.16 Entities のみ
        assert_eq!(
            region_chunk_with_entities(&chunk_legacy(Some(2586), Some(vec![chest()]), vec![])),
            Ok(Some((-1, 2)))
        );
        // 1.17 (Entities 無し)
        assert_eq!(
            region_chunk_with_entities(&chunk_legacy(Some(2730), None, vec![])),
            Ok(None)
        );
    }

    #[test]
    fn broken_list_does_not_hide_other_list() {
        let mut c = chunk_legacy(Some(2586), Some(vec![chest()]), vec![]);
        set(level_mut(&mut c), "TileEntities", Tag::Int(0));
        assert_eq!(region_chunk_with_entities(&c), Ok(Some((-1, 2))));

        set(level_mut(&mut c), "Entities", Tag::List(vec![]));
        assert_eq!(
            region_chunk_with_entities(&c),
            Err("Tag TileEntities has type TAG_Int".to_string())
        );
    }

    #[test]
    fn missing_level_in_legacy_is_error() {
        let c = compound([("DataVersion", Tag::Int(2843))]);
        assert_eq!(
            region_chunk_with_entities(&c),
            Err("Tag Level not found".to_string())
        );
    }

    #[test]
    fn reads_entity_chunks() {
        let mut c = compound([
            ("Position", Tag::IntArray(vec![-5, 7])),
            ("Entities", Tag::List(vec![chest()])),
        ]);
        assert_eq!(entity_chunk_with_entities(&c), Ok(Some((-5, 7))));

        set(&mut c, "Entities", Tag::List(vec![]));
        assert_eq!(entity_chunk_with_entities(&c), Ok(None));
    }
}
