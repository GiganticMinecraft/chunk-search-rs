extern crate protobuf_codegen_pure;

use std::fs::DirBuilder;

fn main() {
    DirBuilder::new()
        .recursive(true)
        .create("src/protos")
        .unwrap();

    protobuf_codegen_pure::Codegen::new()
        .out_dir("src/protos")
        .inputs(&["protocol/chunk-search.proto"])
        .include("protocol")
        .run()
        .expect("protoc");
}
