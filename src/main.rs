mod protos;

use crate::protos::chunk_search::{Chunk, ChunkCoord, SearchResult};
use anvil_region::{
    provider::{FolderRegionProvider, RegionProvider},
    region::Region,
};
use clap::{App, Arg};
use crossbeam_channel::bounded;
use nbt::CompoundTag;
use protobuf::{Message, RepeatedField};
use std::env;
use std::fs::File;
use std::io::stdout;
use std::path::Path;

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

fn list_chunks_with_entities_in_region(region: &mut Region<File>) -> Vec<ChunkCoordinate> {
    let mut result = Vec::new();

    for chunk in region.read_all_chunks().unwrap() {
        if let Some(c) = get_coordinate_if_contains_entities(&chunk).unwrap() {
            result.push(c)
        }
    }

    result
}

fn list_chunks_in_region_folder(
    region_folder_path: &Path,
    worker_count: u16,
) -> Vec<ChunkCoordinate> {
    let (snd_region, rcv_region) = bounded(1);
    let (snd_search_result, rcv_search_result) = bounded(1);

    crossbeam::scope(|s| {
        let region_provider = FolderRegionProvider::new(region_folder_path.to_str().unwrap());
        s.spawn(move |_| {
            region_provider
                .iter_positions()
                .unwrap()
                .map(|position| region_provider.get_region(position).unwrap())
                .for_each(|region| {
                    let _ = snd_region.send(region);
                });

            drop(snd_region);
        });

        for _ in 0..worker_count {
            let (sndsr, rcvrg) = (snd_search_result.clone(), rcv_region.clone());
            s.spawn(move |_| {
                rcvrg.iter().for_each(|mut region| {
                    let result = list_chunks_with_entities_in_region(&mut region);
                    let _ = sndsr.send(result);
                })
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
    let threads = matches
        .value_of("threads")
        .and_then(|t| t.parse::<u16>().ok())
        .unwrap_or(1)
        .max(1);
    let region_folder_path = matches
        .value_of("world_folder")
        .map(|s| Path::new(s).join("region"))
        .unwrap();

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
