//! Anvil region (`r.<x>.<z>.mca`) の最小リーダー。
//!
//! フォーマット: <https://minecraft.wiki/w/Region_file_format>
//!
//! - 先頭 4KiB: 1024 チャンク分の location (上位 3 byte = セクタオフセット、下位 1 byte = セクタ数)
//! - 次の 4KiB: タイムスタンプ (使わない)
//! - 以降 4KiB セクタ単位のチャンクデータ: 長さ (u32 BE, 圧縮種別の 1 byte を含む)、圧縮種別 (u8)、本体
//!
//! 圧縮種別の bit 128 が立っている場合、本体は同じディレクトリの `c.<chunk x>.<chunk z>.mcc`
//! に外部ファイル化されている (下位 bit が実際の圧縮種別)。

use nbt::CompoundTag;
use nbt::decode::{read_compound_tag, read_gzip_compound_tag, read_zlib_compound_tag};
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

const SECTOR_SIZE: usize = 4096;
const HEADER_SIZE: usize = SECTOR_SIZE * 2;
const CHUNKS_PER_REGION_SIDE: i32 = 32;
const EXTERNAL_FLAG: u8 = 128;

/// region ファイル単位で処理を続けられないエラー。
#[derive(Debug)]
pub enum FatalError {
    Io {
        path: PathBuf,
        source: io::Error,
    },
    TruncatedHeader {
        path: PathBuf,
        len: usize,
    },
    UnsupportedCompression {
        path: PathBuf,
        local: (i32, i32),
        scheme: u8,
    },
}

impl fmt::Display for FatalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FatalError::Io { path, source } => {
                write!(f, "{}: I/O error: {}", path.display(), source)
            }
            FatalError::TruncatedHeader { path, len } => write!(
                f,
                "{}: region header is truncated ({} bytes, expected at least {})",
                path.display(),
                len,
                HEADER_SIZE
            ),
            FatalError::UnsupportedCompression {
                path,
                local: (lx, lz),
                scheme,
            } => write!(
                f,
                "{}: chunk at local ({}, {}) uses unsupported compression scheme {}",
                path.display(),
                lx,
                lz,
                scheme
            ),
        }
    }
}

/// 1 チャンクだけを読み飛ばせば済む問題 (壊れたチャンクなど)。
#[derive(Debug)]
pub struct ChunkWarning {
    pub path: PathBuf,
    pub local: (i32, i32),
    pub reason: String,
}

impl fmt::Display for ChunkWarning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}: skipped chunk at local ({}, {}): {}",
            self.path.display(),
            self.local.0,
            self.local.1,
            self.reason
        )
    }
}

/// `r.<x>.<z>.mca` からリージョン座標を取り出す。形式に合わないファイル名は `None`。
pub fn parse_region_file_name(name: &str) -> Option<(i32, i32)> {
    let rest = name.strip_prefix("r.")?.strip_suffix(".mca")?;
    let (x, z) = rest.split_once('.')?;
    Some((x.parse().ok()?, z.parse().ok()?))
}

/// region ファイルのパスとリージョン座標。
pub type RegionFile = (PathBuf, (i32, i32));

/// ディレクトリ内の region ファイルを列挙する (名前順)。
pub fn list_region_files(dir: &Path) -> Result<Vec<RegionFile>, FatalError> {
    let io_err = |source| FatalError::Io {
        path: dir.to_path_buf(),
        source,
    };
    let mut files = Vec::new();
    for entry in fs::read_dir(dir).map_err(io_err)? {
        let entry = entry.map_err(io_err)?;
        let name = entry.file_name();
        let Some(coords) = name.to_str().and_then(parse_region_file_name) else {
            continue;
        };
        if entry.file_type().map_err(io_err)?.is_file() {
            files.push((entry.path(), coords));
        }
    }
    files.sort();
    Ok(files)
}

fn decode(scheme: u8, mut data: &[u8]) -> Result<CompoundTag, String> {
    let result = match scheme {
        1 => read_gzip_compound_tag(&mut data),
        2 => read_zlib_compound_tag(&mut data),
        3 => read_compound_tag(&mut data),
        _ => unreachable!("checked by caller"),
    };
    result.map_err(|e| match e {
        nbt::decode::TagDecodeError::IOError { io_error } => {
            format!("NBT decode error: {io_error}")
        }
        e => format!("NBT decode error: {e}"),
    })
}

/// region ファイルの全チャンクを読み、デコードできたものを `on_chunk` に渡す。
///
/// 読み取り専用で開く。壊れたチャンクは `ChunkWarning` として返し、処理を続ける。
pub fn for_each_chunk(
    path: &Path,
    region: (i32, i32),
    mut on_chunk: impl FnMut((i32, i32), CompoundTag),
) -> Result<Vec<ChunkWarning>, FatalError> {
    let data = fs::read(path).map_err(|source| FatalError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let mut warnings = Vec::new();

    // 0 byte の region ファイルはサーバーが作成直後に落ちた場合などに残る。中身が無いだけなので空扱い。
    if data.is_empty() {
        return Ok(warnings);
    }
    if data.len() < HEADER_SIZE {
        return Err(FatalError::TruncatedHeader {
            path: path.to_path_buf(),
            len: data.len(),
        });
    }

    for index in 0..1024usize {
        let local = ((index % 32) as i32, (index / 32) as i32);
        let mut warn = |reason: String| {
            warnings.push(ChunkWarning {
                path: path.to_path_buf(),
                local,
                reason,
            })
        };

        let loc = u32::from_be_bytes(data[index * 4..index * 4 + 4].try_into().unwrap());
        if loc == 0 {
            continue; // 未生成
        }
        let offset = (loc >> 8) as usize * SECTOR_SIZE;
        if offset < HEADER_SIZE {
            warn(format!("invalid sector offset {}", loc >> 8));
            continue;
        }
        let Some(head) = data.get(offset..offset + 5) else {
            warn(format!("sector offset {} is beyond end of file", loc >> 8));
            continue;
        };
        let length = u32::from_be_bytes(head[0..4].try_into().unwrap()) as usize;
        let compression = head[4];
        if length == 0 {
            warn("chunk length is zero".to_string());
            continue;
        }
        let scheme = compression & !EXTERNAL_FLAG;
        if !(1..=3).contains(&scheme) {
            return Err(FatalError::UnsupportedCompression {
                path: path.to_path_buf(),
                local,
                scheme: compression,
            });
        }

        let nbt = if compression & EXTERNAL_FLAG != 0 {
            let cx = region.0 * CHUNKS_PER_REGION_SIDE + local.0;
            let cz = region.1 * CHUNKS_PER_REGION_SIDE + local.1;
            let external = path.with_file_name(format!("c.{cx}.{cz}.mcc"));
            match fs::read(&external) {
                Ok(bytes) => decode(scheme, &bytes),
                // 参照先が無いのはチャンク単位の破損 (Minecraft 自身もそのチャンクを読めない)。
                Err(e) if e.kind() == io::ErrorKind::NotFound => Err(format!(
                    "external chunk file {} not found",
                    external.display()
                )),
                Err(source) => {
                    return Err(FatalError::Io {
                        path: external,
                        source,
                    });
                }
            }
        } else {
            match data.get(offset + 5..offset + 4 + length) {
                Some(body) => decode(scheme, body),
                None => Err(format!("chunk data ({length} bytes) runs past end of file")),
            }
        };

        match nbt {
            Ok(nbt) => on_chunk(local, nbt),
            Err(reason) => warn(reason),
        }
    }

    Ok(warnings)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_region_file_names() {
        assert_eq!(parse_region_file_name("r.0.0.mca"), Some((0, 0)));
        assert_eq!(parse_region_file_name("r.-1.12.mca"), Some((-1, 12)));
        assert_eq!(parse_region_file_name("r.0.0.mcr"), None);
        assert_eq!(parse_region_file_name("c.0.0.mcc"), None);
        assert_eq!(parse_region_file_name("r.0.mca"), None);
        assert_eq!(parse_region_file_name("r.a.0.mca"), None);
    }
}
