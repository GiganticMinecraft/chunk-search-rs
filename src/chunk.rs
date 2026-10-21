//! チャンク NBT からエンティティ / ブロックエンティティの有無を判定する。

use nbt::{CompoundTag, CompoundTagError};

/// 21w43a (1.18 開発版)。これ以降のチャンクは `Level` が無くなり、フィールドがルート直下に置かれる。
pub const DATA_VERSION_21W43A: i32 = 2844;

fn optional<T>(result: Result<T, CompoundTagError<'_>>) -> Result<Option<T>, String> {
    match result {
        Ok(v) => Ok(Some(v)),
        Err(CompoundTagError::TagNotFound { .. }) => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}

/// リスト型タグが存在して 1 要素以上あるか。存在しなければ false。
fn has_entries(tag: &CompoundTag, name: &str) -> Result<bool, String> {
    Ok(optional(tag.get_compound_tag_vec(name))?.is_some_and(|v| !v.is_empty()))
}

/// `region/` のチャンク NBT を調べ、エンティティかブロックエンティティを含むならチャンク座標を返す。
///
/// - DataVersion >= 2844 (1.18 形式): ルート直下の `xPos`/`zPos`/`block_entities`
///   (未完成チャンクの `entities` も見る)
/// - それ未満または DataVersion 無し (1.17 以前): `Level` 配下の `xPos`/`zPos`/`Entities`/`TileEntities`。
///   1.17 では `Entities` が `entities/` に移ったので無くてもよい。
pub fn region_chunk_with_entities(chunk: &CompoundTag) -> Result<Option<(i32, i32)>, String> {
    let data_version = optional(chunk.get_i32("DataVersion"))?;

    let (base, entity_lists): (&CompoundTag, &[&str]) = match data_version {
        Some(v) if v >= DATA_VERSION_21W43A => (chunk, &["block_entities", "entities"]),
        _ => (
            chunk.get_compound_tag("Level").map_err(|e| e.to_string())?,
            &["TileEntities", "Entities"],
        ),
    };

    let x = base.get_i32("xPos").map_err(|e| e.to_string())?;
    let z = base.get_i32("zPos").map_err(|e| e.to_string())?;

    for name in entity_lists {
        if has_entries(base, name)? {
            return Ok(Some((x, z)));
        }
    }
    Ok(None)
}

/// `entities/` (1.17 以降) のチャンク NBT を調べ、エンティティを含むならチャンク座標を返す。
pub fn entity_chunk_with_entities(chunk: &CompoundTag) -> Result<Option<(i32, i32)>, String> {
    if !has_entries(chunk, "Entities")? {
        return Ok(None);
    }
    let position = chunk.get_i32_vec("Position").map_err(|e| e.to_string())?;
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

    fn chest() -> CompoundTag {
        let mut c = CompoundTag::new();
        c.insert_str("id", "minecraft:chest");
        c
    }

    fn chunk_118(be: Vec<CompoundTag>) -> CompoundTag {
        let mut c = CompoundTag::new();
        c.insert_i32("DataVersion", 2975);
        c.insert_i32("xPos", 3);
        c.insert_i32("zPos", -4);
        c.insert_compound_tag_vec("block_entities", be);
        c
    }

    fn chunk_legacy(
        data_version: Option<i32>,
        entities: Option<Vec<CompoundTag>>,
        te: Vec<CompoundTag>,
    ) -> CompoundTag {
        let mut level = CompoundTag::new();
        level.insert_i32("xPos", -1);
        level.insert_i32("zPos", 2);
        if let Some(e) = entities {
            level.insert_compound_tag_vec("Entities", e);
        }
        level.insert_compound_tag_vec("TileEntities", te);
        let mut c = CompoundTag::new();
        if let Some(v) = data_version {
            c.insert_i32("DataVersion", v);
        }
        c.insert_compound_tag("Level", level);
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
    fn missing_level_in_legacy_is_error() {
        let mut c = CompoundTag::new();
        c.insert_i32("DataVersion", 2843);
        assert!(region_chunk_with_entities(&c).is_err());
    }

    #[test]
    fn reads_entity_chunks() {
        let mut c = CompoundTag::new();
        c.insert_i32_vec("Position", vec![-5, 7]);
        c.insert_compound_tag_vec("Entities", vec![chest()]);
        assert_eq!(entity_chunk_with_entities(&c), Ok(Some((-5, 7))));

        c.insert_compound_tag_vec("Entities", vec![]);
        assert_eq!(entity_chunk_with_entities(&c), Ok(None));
    }
}
