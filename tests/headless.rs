//! End-to-end proof that a decoded libmpv frame reaches a Rune snapshot.

use std::path::PathBuf;
use std::process::Command;

#[test]
fn a_headless_snap_contains_a_video_frame() {
    let png = std::env::temp_dir().join(format!("kachito-headless-{}.png", std::process::id()));
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/test/rgbw.mkv");
    let status = Command::new(env!("CARGO_BIN_EXE_kachito"))
        .arg("--snap")
        .arg(&png)
        .arg(&source)
        .status()
        .expect("run the Kachito binary");
    assert!(status.success(), "the headless snapshot failed");

    let file = std::fs::File::open(&png).expect("open the snapshot");
    let decoder = png::Decoder::new(std::io::BufReader::new(file));
    let mut reader = decoder.read_info().expect("read the PNG header");
    let mut pixels = vec![0; reader.output_buffer_size().expect("PNG size")];
    let info = reader.next_frame(&mut pixels).expect("decode the PNG");

    let x = info.width / 2;
    let y = info.height / 2;
    let index = (y * info.width + x) as usize * 4;
    let brightest = pixels[index..index + 3].iter().copied().max().unwrap();
    assert!(
        brightest > 180,
        "the centre pixel is {brightest}, so the snapshot has no video frame"
    );
}
