mod protos;

use crate::protos::chunk_search::{Chunk, ChunkCoord, SearchResult};
use anvil_region::AnvilRegion;
use clap::{App, Arg};
use crossbeam_channel::bounded;
use nbt::CompoundTag;
use protobuf::{Message, RepeatedField};
use std::env;
use std::fs::OpenOptions;
use std::io::{stdout, Cursor, Read};
use std::path::{Path, PathBuf};

#[derive(Debug)]
struct ChunkCoordinate {
    x: i32,
    z: i32,
}

impl From<&ChunkCoordinate> for Chunk {
    fn from(cc: &ChunkCoordinate) -> Self {
        let mut chunk = Chunk::new();
        let mut coord = ChunkCoord::new();
        coord.set_x(cc.x);
        coord.set_z(cc.z);
        chunk.set_coord(coord);
        chunk
    }
}

fn get_coordinate_if_contains_entities(
    chunk_nbt: &CompoundTag,
) -> Result<Option<ChunkCoordinate>, nbt::CompoundTagError> {
    let level = chunk_nbt.get_compound_tag("Level")?;

    let x = level.get_i32("xPos")?;
    let z = level.get_i32("zPos")?;

    let chunk_contains_entity = !level.get_compound_tag_vec("Entities")?.is_empty();
    let chunk_contains_tile_entity = !level.get_compound_tag_vec("TileEntities")?.is_empty();

    let result = if chunk_contains_entity || chunk_contains_tile_entity {
        Some(ChunkCoordinate { x, z })
    } else {
        None
    };

    Ok(result)
}

fn get_anvil_region_instance(
    region_file_path: &Path,
) -> std::io::Result<AnvilRegion<Cursor<Vec<u8>>>> {
    let file_contents = {
        let mut region_file = OpenOptions::new()
            .read(true)
            .write(false)
            .create(false)
            .open(region_file_path)?;
        let mut contents = Vec::new();
        region_file.read_to_end(&mut contents)?;
        contents
    };

    let region = AnvilRegion::new(Cursor::new(file_contents))?;
    Ok(region)
}

fn list_chunks_with_entities_in_region(region_file: &PathBuf) -> Vec<ChunkCoordinate> {
    let mut result = Vec::new();
    let mut region = get_anvil_region_instance(&region_file).unwrap();

    for chunk in region.read_all_chunks().unwrap() {
        if let Some(c) = get_coordinate_if_contains_entities(&chunk).unwrap() {
            result.push(c)
        }
    }

    result
}

fn list_chunks_in_region_folder(
    region_folder_path: &PathBuf,
    worker_count: u16,
) -> Vec<ChunkCoordinate> {
    let (snd_region_file_path, rcv_region_file_path) = bounded(1);
    let (snd_search_result, rcv_search_result) = bounded(1);

    crossbeam::scope(|s| {
        let region_folder_path = region_folder_path.clone();
        s.spawn(move |_| {
            for region_file in region_folder_path.read_dir().unwrap() {
                let region_file = region_file.unwrap().path();
                let _ = snd_region_file_path.send(region_file);
            }

            drop(snd_region_file_path);
        });

        for _ in 0..worker_count {
            let (sndsr, rcvfp) = (snd_search_result.clone(), rcv_region_file_path.clone());
            s.spawn(move |_| {
                for path in rcvfp.iter() {
                    let result = list_chunks_with_entities_in_region(&path);
                    let _ = sndsr.send(result);
                }
            });
        }

        drop(snd_search_result);

        rcv_search_result.iter().flatten().collect::<Vec<_>>()
    })
    .unwrap()
}

fn main() {
    let app = App::new(clap::crate_name!())
        .version(clap::crate_version!())
        .author(clap::crate_authors!())
        .about(clap::crate_description!())
        .arg(
            Arg::with_name("protobuf")
                .help("Enables protobuf-compiled output")
                .short('p')
                .long("protobuf"),
        )
        .arg(
            Arg::with_name("threads")
                .help("Number of threads used to process region files")
                .short('t')
                .long("threads")
                .takes_value(true),
        )
        .arg(
            Arg::with_name("world_folder")
                .help("Location containing world data to be traversed e.g. /spigot/world")
                .required(true),
        );

    let matches = app.get_matches();

    let use_protobuf = matches.is_present("protobuf");
    let threads = match matches.value_of("threads") {
        Some(t) => t.parse::<u16>().unwrap_or(1).max(1),
        None => 1,
    };

    let world_folder_path_str = matches.value_of("world_folder").unwrap();
    let world_folder_path: &Path = Path::new(&world_folder_path_str);
    let region_folder_path = world_folder_path.join("region");

    let result = list_chunks_in_region_folder(&region_folder_path, threads);

    if use_protobuf {
        let mut search_result: SearchResult = protos::chunk_search::SearchResult::new();
        {
            let converted_result = result.iter().map(Chunk::from).collect::<Vec<_>>();
            search_result.set_result(RepeatedField::from(converted_result))
        }
        search_result.write_to_writer(&mut stdout()).unwrap();
    } else {
        for ChunkCoordinate { x, z } in result {
            println!("({}, {})", x, z);
        }
    }
}
