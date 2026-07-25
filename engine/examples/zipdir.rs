//! Dev helper: zip a directory into an OOXML package with forward-slash names.
//! Run: cargo run --release --example zipdir -- <src_dir> <out.pptx>

use std::io::Write;
use std::path::Path;

fn main() {
    let src = std::env::args().nth(1).expect("usage: zipdir <src_dir> <out>");
    let out = std::env::args().nth(2).expect("usage: zipdir <src_dir> <out>");
    let file = std::fs::File::create(&out).expect("create out");
    let mut zip = zip::ZipWriter::new(file);
    let opts: zip::write::FileOptions<()> =
        zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    let root = Path::new(&src);
    let mut count = 0;
    walk(root, root, &mut zip, &opts, &mut count);
    zip.finish().expect("finish");
    println!("zipped {count} entries into {out}");
}

fn walk(
    root: &Path,
    dir: &Path,
    zip: &mut zip::ZipWriter<std::fs::File>,
    opts: &zip::write::FileOptions<()>,
    count: &mut u32,
) {
    for entry in std::fs::read_dir(dir).expect("read_dir") {
        let entry = entry.expect("entry");
        let path = entry.path();
        if path.is_dir() {
            walk(root, &path, zip, opts, count);
        } else {
            let rel = path.strip_prefix(root).unwrap();
            let name = rel.to_string_lossy().replace('\\', "/");
            zip.start_file(name, *opts).expect("start_file");
            let bytes = std::fs::read(&path).expect("read file");
            zip.write_all(&bytes).expect("write");
            *count += 1;
        }
    }
}
