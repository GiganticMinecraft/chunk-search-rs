//! `tests/fixtures/worlds` (tests/fixtures/gen.py で生成) に対してバイナリを実行する統合テスト。

#[path = "../src/protos/mod.rs"]
mod protos;

use protobuf::Message;
use protos::chunk_search::SearchResult;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/worlds")
        .join(name)
}

fn run(world: &Path, protobuf: bool) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_chunk-search-rs"));
    if protobuf {
        cmd.arg("--protobuf");
    }
    cmd.args(["--threads", "4"]).arg(world).output().unwrap()
}

fn parse_text(stdout: &[u8]) -> Vec<(i32, i32)> {
    String::from_utf8(stdout.to_vec())
        .unwrap()
        .lines()
        .map(|line| {
            let (x, z) = line
                .strip_prefix('(')
                .and_then(|l| l.strip_suffix(')'))
                .and_then(|l| l.split_once(", "))
                .unwrap_or_else(|| panic!("unexpected line: {line:?}"));
            (x.parse().unwrap(), z.parse().unwrap())
        })
        .collect()
}

fn parse_protobuf(stdout: &[u8]) -> Vec<(i32, i32)> {
    SearchResult::parse_from_bytes(stdout)
        .unwrap()
        .result
        .iter()
        .map(|c| (c.coord.x, c.coord.z))
        .collect()
}

/// テキスト / protobuf の両方で実行し、成功して期待どおりのチャンクを返すことを確認する。
/// stderr を返す。
fn assert_chunks(world: &Path, expected: &[(i32, i32)]) -> String {
    let text = run(world, false);
    let stderr = String::from_utf8_lossy(&text.stderr).into_owned();
    assert!(
        text.status.success(),
        "{world:?}: {:?} {stderr}",
        text.status
    );
    assert_eq!(parse_text(&text.stdout), expected, "{world:?} (text)");

    let pb = run(world, true);
    assert!(pb.status.success(), "{world:?}: {:?}", pb.status);
    assert_eq!(parse_protobuf(&pb.stdout), expected, "{world:?} (protobuf)");
    assert_eq!(String::from_utf8_lossy(&pb.stderr), stderr);

    stderr
}

/// テキスト / protobuf の両方で非 0 終了し、stdout が空であることを確認する。stderr を返す。
fn assert_fails(world: &Path) -> String {
    let mut stderr = String::new();
    for protobuf in [false, true] {
        let out = run(world, protobuf);
        assert_eq!(out.status.code(), Some(1), "{world:?} protobuf={protobuf}");
        assert!(out.stdout.is_empty(), "{world:?} protobuf={protobuf}");
        stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        assert!(stderr.contains("error: "), "{stderr}");
    }
    stderr
}

fn scratch_world(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(name);
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn legacy_formats() {
    assert_eq!(assert_chunks(&fixture("w112"), &[(0, 0)]), "");
    assert_eq!(assert_chunks(&fixture("w116"), &[(0, 0)]), "");
    assert_eq!(assert_chunks(&fixture("w116_empty"), &[]), "");
    // 1.17: Level.Entities が無くても落ちない
    assert_eq!(assert_chunks(&fixture("w117"), &[(0, 0)]), "");
}

#[test]
fn format_1_18() {
    assert_eq!(assert_chunks(&fixture("w1182"), &[(0, 0)]), "");
    assert_eq!(
        assert_chunks(&fixture("w1182_mixed"), &[(0, 0), (1, 0)]),
        ""
    );
    assert_eq!(assert_chunks(&fixture("w1182_external"), &[(0, 0)]), "");
    assert_eq!(
        assert_chunks(&fixture("w1182_compression"), &[(0, 0), (1, 0)]),
        ""
    );
    assert_eq!(assert_chunks(&fixture("w_nether"), &[(0, 0)]), "");
}

#[test]
fn entities_folder() {
    assert_eq!(
        assert_chunks(&fixture("w1182_entities_only"), &[(0, 0)]),
        ""
    );
    // region 側と entities 側の和集合 (重複は 1 つ)、負のリージョン座標、外部ファイル
    assert_eq!(
        assert_chunks(&fixture("w1182_negative"), &[(-2, -3), (-2, -1), (-1, -1)]),
        ""
    );
}

#[test]
fn broken_chunks_are_skipped_with_warning() {
    let stderr = assert_chunks(&fixture("w1182_corrupt"), &[(0, 0)]);
    assert_eq!(stderr.lines().count(), 2, "{stderr}");
    assert!(stderr.contains("local (1, 0)"), "{stderr}");
    assert!(stderr.contains("local (2, 0)"), "{stderr}");
    assert!(stderr.contains("xPos"), "{stderr}");

    let stderr = assert_chunks(&fixture("w1182_missing_external"), &[(0, 0)]);
    assert!(stderr.contains("c.1.0.mcc"), "{stderr}");
}

#[test]
fn unknown_compression_fails() {
    let stderr = assert_fails(&fixture("w1182_unknown_compression"));
    assert!(stderr.contains("compression scheme 4\n"), "{stderr}");

    // 外部ファイル化された LZ4 チャンク (132 = 4 | 128)
    let world = scratch_world("w_external_lz4");
    fs::create_dir(world.join("region")).unwrap();
    let mut data = fs::read(fixture("w1182/region/r.0.0.mca")).unwrap();
    data[8192 + 4] = 4 | 128;
    fs::write(world.join("region/r.0.0.mca"), data).unwrap();
    let stderr = assert_fails(&world);
    assert!(
        stderr.contains("compression scheme 4 (external)"),
        "{stderr}"
    );
}

#[test]
fn missing_region_folder_fails() {
    let world = scratch_world("w_noregion");
    let stderr = assert_fails(&world);
    assert!(stderr.contains("region directory not found"), "{stderr}");
}

#[test]
fn truncated_region_header_is_warning() {
    let world = scratch_world("w_truncated");
    fs::create_dir(world.join("region")).unwrap();
    fs::copy(
        fixture("w1182/region/r.0.0.mca"),
        world.join("region/r.0.0.mca"),
    )
    .unwrap();
    // ヘッダが 100 byte で切れた region (読めた location は 0 なので中身は無い)
    fs::write(world.join("region/r.1.0.mca"), [0u8; 100]).unwrap();
    let stderr = assert_chunks(&world, &[(0, 0)]);
    assert!(
        stderr.contains("r.1.0.mca: region header is truncated"),
        "{stderr}"
    );
}

#[test]
fn empty_region_file_and_other_files_are_ignored() {
    let world = scratch_world("w_empty_file");
    fs::create_dir(world.join("region")).unwrap();
    fs::copy(
        fixture("w1182/region/r.0.0.mca"),
        world.join("region/r.0.0.mca"),
    )
    .unwrap();
    fs::write(world.join("region/r.1.0.mca"), []).unwrap();
    fs::write(world.join("region/r.0.0.mca.bak"), [0u8; 100]).unwrap();
    assert_eq!(assert_chunks(&world, &[(0, 0)]), "");
}

#[test]
fn warnings_are_capped() {
    // 1024 チャンクすべてが壊れた region
    let world = scratch_world("w_many_warnings");
    fs::create_dir(world.join("region")).unwrap();
    let mut data = vec![0u8; 8192];
    for i in 0..1024u32 {
        data[i as usize * 4..i as usize * 4 + 4].copy_from_slice(&((2 + i) << 8 | 1).to_be_bytes());
    }
    for _ in 0..1024 {
        let mut sector = vec![0u8; 4096];
        sector[0..4].copy_from_slice(&5u32.to_be_bytes());
        sector[4] = 2;
        sector[5..9].copy_from_slice(b"junk");
        data.extend(sector);
    }
    fs::write(world.join("region/r.0.0.mca"), data).unwrap();

    let stderr = assert_chunks(&world, &[]);
    assert_eq!(stderr.lines().count(), 101, "{stderr}");
    assert!(
        stderr.ends_with("warning: 924 more messages were suppressed\n"),
        "{stderr}"
    );
}

/// 1 チャンクだけ入った region を作る。`patch` でヘッダ / チャンクデータを書き換える。
fn single_chunk_region(world: &Path, patch: impl FnOnce(&mut Vec<u8>)) {
    let mut data = fs::read(fixture("w1182/region/r.0.0.mca")).unwrap();
    patch(&mut data);
    fs::write(world.join("region/r.1.0.mca"), data).unwrap();
}

#[test]
fn broken_chunk_headers_are_skipped_with_warning() {
    let world = scratch_world("w_broken_headers");
    fs::create_dir(world.join("region")).unwrap();
    fs::copy(
        fixture("w1182/region/r.0.0.mca"),
        world.join("region/r.0.0.mca"),
    )
    .unwrap();
    // 圧縮種別 0 (存在しない値) は壊れたチャンク扱い
    single_chunk_region(&world, |d| d[8192 + 4] = 0);
    let stderr = assert_chunks(&world, &[(0, 0)]);
    assert!(stderr.contains("invalid compression scheme 0"), "{stderr}");

    // セクタ数 0 の location
    single_chunk_region(&world, |d| d[3] = 0);
    let stderr = assert_chunks(&world, &[(0, 0)]);
    assert!(stderr.contains("sector count is zero"), "{stderr}");

    // length が確保されたセクタを超える
    single_chunk_region(&world, |d| {
        d[8192..8196].copy_from_slice(&5000u32.to_be_bytes());
        d.extend([0u8; 4096]);
    });
    let stderr = assert_chunks(&world, &[(0, 0)]);
    assert!(stderr.contains("allocated sectors"), "{stderr}");
}

#[test]
fn empty_top_level_region_does_not_hide_nether() {
    let world = scratch_world("w_nether_with_empty_region");
    fs::create_dir(world.join("region")).unwrap();
    fs::create_dir_all(world.join("DIM-1/region")).unwrap();
    fs::copy(
        fixture("w1182_mixed/region/r.0.0.mca"),
        world.join("DIM-1/region/r.0.0.mca"),
    )
    .unwrap();
    assert_eq!(assert_chunks(&world, &[(0, 0), (1, 0)]), "");
}

#[cfg(unix)]
#[test]
fn symlinked_region_files_are_read() {
    let world = scratch_world("w_symlink");
    fs::create_dir(world.join("region")).unwrap();
    std::os::unix::fs::symlink(
        fixture("w1182/region/r.0.0.mca"),
        world.join("region/r.0.0.mca"),
    )
    .unwrap();
    // 非正規の名前のコピーは読まない
    fs::copy(
        fixture("w1182_mixed/region/r.0.0.mca"),
        world.join("region/r.00.0.mca"),
    )
    .unwrap();
    assert_eq!(assert_chunks(&world, &[(0, 0)]), "");
}

#[test]
fn vanilla_layout_searches_overworld_only() {
    let world = scratch_world("w_vanilla_layout");
    fs::create_dir(world.join("region")).unwrap();
    fs::create_dir_all(world.join("DIM-1/region")).unwrap();
    fs::copy(
        fixture("w1182/region/r.0.0.mca"),
        world.join("region/r.0.0.mca"),
    )
    .unwrap();
    fs::copy(
        fixture("w1182_negative/region/r.-1.-1.mca"),
        world.join("DIM-1/region/r.-1.-1.mca"),
    )
    .unwrap();
    assert_eq!(assert_chunks(&world, &[(0, 0)]), "");
}

#[test]
fn ambiguous_dimension_fails() {
    let world = scratch_world("w_ambiguous_dimension");
    for dim in ["DIM-1", "DIM1"] {
        fs::create_dir_all(world.join(dim).join("region")).unwrap();
        fs::copy(
            fixture("w1182/region/r.0.0.mca"),
            world.join(dim).join("region/r.0.0.mca"),
        )
        .unwrap();
    }
    let stderr = assert_fails(&world);
    assert!(stderr.contains("cannot decide"), "{stderr}");
}

#[cfg(unix)]
#[test]
fn dangling_symlink_fails_with_file_path() {
    let world = scratch_world("w_dangling_symlink");
    fs::create_dir(world.join("region")).unwrap();
    std::os::unix::fs::symlink(world.join("missing.mca"), world.join("region/r.0.0.mca")).unwrap();
    let stderr = assert_fails(&world);
    assert!(stderr.contains("region/r.0.0.mca: I/O error"), "{stderr}");
}
