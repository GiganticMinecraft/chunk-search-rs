mod chunk;
mod nbt;
mod protos;
mod region;

use crate::protos::chunk_search::{Chunk, ChunkCoord, SearchResult};
use crate::region::{ChunkWarning, FatalError, RegionFile};
use clap::Parser;
use crossbeam_channel::bounded;
use protobuf::{Message, MessageField};
use std::collections::BTreeSet;
use std::fmt::Display;
use std::io::{Write, stdout};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};

/// stderr に出す警告・エラーそれぞれの上限。
///
/// SeichiAssist は `Runtime.exec` で起動して stdout しか読まないため、stderr に大量に書くと
/// パイプバッファが埋まってこのプロセスが書き込みでブロックし、双方が止まる。
const MAX_PRINTED_MESSAGES: usize = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct ChunkCoordinate {
    x: i32,
    z: i32,
}

impl From<&ChunkCoordinate> for Chunk {
    fn from(cc: &ChunkCoordinate) -> Self {
        let mut chunk = Chunk::new();
        let mut coord = ChunkCoord::new();
        coord.x = cc.x;
        coord.z = cc.z;
        chunk.coord = MessageField::some(coord);

        chunk
    }
}

#[derive(Debug, Clone, Copy)]
enum FileKind {
    /// `region/r.*.mca`: 地形・ブロックエンティティ (1.16 以前はエンティティも)
    Region,
    /// `entities/r.*.mca`: 1.17 以降のエンティティ
    Entities,
}

struct Job {
    path: PathBuf,
    region: (i32, i32),
    kind: FileKind,
}

struct JobOutput {
    chunks: Vec<ChunkCoordinate>,
    warnings: Vec<ChunkWarning>,
}

fn process_file(job: &Job) -> Result<JobOutput, FatalError> {
    let mut chunks = Vec::new();
    let mut nbt_warnings = Vec::new();

    let mut warnings = region::for_each_chunk(&job.path, job.region, |local, nbt| {
        let found = match job.kind {
            FileKind::Region => chunk::region_chunk_with_entities(&nbt),
            FileKind::Entities => chunk::entity_chunk_with_entities(&nbt),
        };
        match found {
            Ok(Some((x, z))) => chunks.push(ChunkCoordinate { x, z }),
            Ok(None) => {}
            Err(reason) => nbt_warnings.push(ChunkWarning {
                path: job.path.clone(),
                local: Some(local),
                reason,
            }),
        }
    })?;
    warnings.append(&mut nbt_warnings);

    Ok(JobOutput { chunks, warnings })
}

/// ワールドフォルダから、ディメンションのデータが置かれたフォルダと region ファイルの一覧を探す。
///
/// 1. `<world>/region` に region ファイルがあればそれ (通常のワールド。vanilla の
///    `<world>/DIM-1` などは別ディメンションなので見ない)
/// 2. 無ければ region ファイルを含む `<world>/DIM-1` か `<world>/DIM1`
///    (Bukkit のネザー / エンドのワールドフォルダ)。両方にあればどちらか決められないのでエラー
/// 3. どこにも region ファイルが無ければ、存在する最初の `region` ディレクトリ (結果は空)
fn find_dimension_folder(world: &Path) -> Result<(PathBuf, Vec<RegionFile>), String> {
    let mut found = Vec::new();
    for dir in [world.to_path_buf(), world.join("DIM-1"), world.join("DIM1")] {
        let region_dir = dir.join("region");
        if region_dir.is_dir() {
            let files = region::list_region_files(&region_dir).map_err(|e| e.to_string())?;
            found.push((dir, files));
        }
    }

    let Some(first) = found.first() else {
        return Err(format!(
            "region directory not found under {}",
            world.display()
        ));
    };
    if first.0 == world && !first.1.is_empty() {
        return Ok(found.swap_remove(0));
    }
    let mut with_files: Vec<_> = found
        .iter()
        .enumerate()
        .filter(|(_, (_, files))| !files.is_empty())
        .map(|(i, _)| i)
        .collect();
    match with_files.len() {
        0 => Ok(found.swap_remove(0)),
        1 => Ok(found.swap_remove(with_files.remove(0))),
        _ => Err(format!(
            "both {}/DIM-1/region and {}/DIM1/region contain region files; cannot decide which dimension to search",
            world.display(),
            world.display()
        )),
    }
}

fn collect_jobs(
    dimension_folder: &Path,
    region_files: Vec<RegionFile>,
) -> Result<Vec<Job>, FatalError> {
    let mut jobs: Vec<Job> = region_files
        .into_iter()
        .map(|(path, region)| Job {
            path,
            region,
            kind: FileKind::Region,
        })
        .collect();

    // 1.16 以前のワールドには entities/ が無い
    let entities = dimension_folder.join("entities");
    if entities.is_dir() {
        for (path, region) in region::list_region_files(&entities)? {
            jobs.push(Job {
                path,
                region,
                kind: FileKind::Entities,
            });
        }
    }
    Ok(jobs)
}

/// 上限付きで stderr にメッセージを出す。
struct LimitedPrinter {
    label: &'static str,
    count: usize,
}

impl LimitedPrinter {
    fn new(label: &'static str) -> Self {
        LimitedPrinter { label, count: 0 }
    }

    fn print(&mut self, message: impl Display) {
        self.count += 1;
        if self.count <= MAX_PRINTED_MESSAGES {
            eprintln!("{}: {}", self.label, message);
        }
    }

    fn finish(&self) {
        if self.count > MAX_PRINTED_MESSAGES {
            eprintln!(
                "{}: {} more messages were suppressed",
                self.label,
                self.count - MAX_PRINTED_MESSAGES
            );
        }
    }
}

struct SearchOutcome {
    chunks: BTreeSet<ChunkCoordinate>,
    fatal_error_count: usize,
}

fn search(jobs: Vec<Job>, worker_count: u16) -> SearchOutcome {
    let (snd_job, rcv_job) = bounded::<Job>(1);
    let (snd_result, rcv_result) = bounded(1);
    // 致命的エラーが出たら結果は捨てるので、残りのファイルは読まない
    let aborted = AtomicBool::new(false);
    let aborted = &aborted;

    crossbeam::scope(|s| {
        s.spawn(move |_| {
            for job in jobs {
                if aborted.load(Ordering::Relaxed) || snd_job.send(job).is_err() {
                    break;
                }
            }
        });

        for _ in 0..worker_count {
            let (snd_result, rcv_job) = (snd_result.clone(), rcv_job.clone());
            s.spawn(move |_| {
                for job in rcv_job.iter() {
                    if aborted.load(Ordering::Relaxed) {
                        continue;
                    }
                    let _ = snd_result.send(process_file(&job));
                }
            });
        }
        drop(snd_result);

        let mut chunks = BTreeSet::new();
        let mut warnings = LimitedPrinter::new("warning");
        let mut errors = LimitedPrinter::new("error");
        for result in rcv_result.iter() {
            match result {
                Ok(output) => {
                    output.warnings.iter().for_each(|w| warnings.print(w));
                    chunks.extend(output.chunks);
                }
                Err(e) => {
                    aborted.store(true, Ordering::Relaxed);
                    errors.print(e);
                }
            }
        }
        warnings.finish();
        errors.finish();

        SearchOutcome {
            chunks,
            fatal_error_count: errors.count,
        }
    })
    .expect("worker thread panicked")
}

#[derive(Parser)]
#[command(author, version, about)]
struct App {
    /// Enables protobuf-compiled output
    #[arg(long, short)]
    protobuf: bool,
    /// Number of threads used to process region files
    #[arg(long, short)]
    threads: Option<u16>,
    /// Location containing world data to be traversed e.g. /spigot/world
    world_folder: String,
}

fn write_output(chunks: &BTreeSet<ChunkCoordinate>, protobuf: bool) -> std::io::Result<()> {
    let mut out = stdout().lock();
    if protobuf {
        let mut search_result = SearchResult::new();
        search_result.result = chunks.iter().map(Chunk::from).collect();
        search_result
            .write_to_writer(&mut out)
            .map_err(std::io::Error::other)?;
    } else {
        for ChunkCoordinate { x, z } in chunks {
            writeln!(out, "({}, {})", x, z)?;
        }
    }
    out.flush()
}

fn main() -> ExitCode {
    let app = App::parse();
    let threads = app.threads.unwrap_or(1).max(1);
    let world_folder = Path::new(&app.world_folder);

    let (dimension_folder, region_files) = match find_dimension_folder(world_folder) {
        Ok(found) => found,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };

    let jobs = match collect_jobs(&dimension_folder, region_files) {
        Ok(jobs) => jobs,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };

    let outcome = search(jobs, threads);
    if outcome.fatal_error_count > 0 {
        // 一部の region を読めていない結果を出すと、呼び出し元はそれを完全な結果として扱ってしまう。
        // stdout には何も出さずに失敗する。
        return ExitCode::FAILURE;
    }

    if let Err(e) = write_output(&outcome.chunks, app.protobuf) {
        eprintln!("error: failed to write output: {e}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}
