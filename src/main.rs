mod chunk;
mod protos;
mod region;

use crate::protos::chunk_search::{Chunk, ChunkCoord, SearchResult};
use crate::region::{ChunkWarning, FatalError};
use clap::Parser;
use crossbeam_channel::bounded;
use protobuf::{Message, MessageField};
use std::collections::BTreeSet;
use std::fmt::Display;
use std::io::{Write, stdout};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

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
                local,
                reason,
            }),
        }
    })?;
    warnings.append(&mut nbt_warnings);

    Ok(JobOutput { chunks, warnings })
}

/// ワールドフォルダからディメンションのデータが置かれたフォルダを探す。
///
/// 通常は `<world>/region`。`region` が無い場合は Bukkit のネザー / エンドのワールドフォルダ
/// (`world_nether/DIM-1/region`, `world_the_end/DIM1/region`) を探す。
fn find_dimension_folder(world: &Path) -> Option<PathBuf> {
    [world.to_path_buf(), world.join("DIM-1"), world.join("DIM1")]
        .into_iter()
        .find(|dir| dir.join("region").is_dir())
}

fn collect_jobs(dimension_folder: &Path) -> Result<Vec<Job>, FatalError> {
    let mut jobs = Vec::new();
    let mut add = |dir: PathBuf, kind: FileKind| -> Result<(), FatalError> {
        for (path, region) in region::list_region_files(&dir)? {
            jobs.push(Job { path, region, kind });
        }
        Ok(())
    };

    add(dimension_folder.join("region"), FileKind::Region)?;
    // 1.16 以前のワールドには entities/ が無い
    let entities = dimension_folder.join("entities");
    if entities.is_dir() {
        add(entities, FileKind::Entities)?;
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

    crossbeam::scope(|s| {
        s.spawn(move |_| {
            for job in jobs {
                if snd_job.send(job).is_err() {
                    break;
                }
            }
        });

        for _ in 0..worker_count {
            let (snd_result, rcv_job) = (snd_result.clone(), rcv_job.clone());
            s.spawn(move |_| {
                for job in rcv_job.iter() {
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
                Err(e) => errors.print(e),
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

    let Some(dimension_folder) = find_dimension_folder(world_folder) else {
        eprintln!(
            "error: region directory not found under {}",
            world_folder.display()
        );
        return ExitCode::FAILURE;
    };

    let jobs = match collect_jobs(&dimension_folder) {
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
