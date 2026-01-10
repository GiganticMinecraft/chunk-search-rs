extern crate protobuf_codegen;

use std::fs::DirBuilder;

fn main() {
    DirBuilder::new()
        .recursive(true)
        .create("src/protos")
        .unwrap();

    protobuf_codegen::Codegen::new()
        // 旧protobuf-codegen-pure相当の設定
        .pure()
        .out_dir("src/protos")
        .inputs(&["protocol/chunk-search.proto"])
        .include("protocol")
        .run()
        .expect("protoc");
}
