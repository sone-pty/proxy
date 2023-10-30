use vnpkt::coder::Language;

fn main() {
    vnpkt::coder::generate_file_by_file("./proto.pkt", Language::Rust, None, "src/proto.rs")
        .unwrap();
}
