use super::*;

/// A small deterministic generator (xorshift).
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 16) as u32
    }
}

/// A test image: a soft colour gradient with an opaque disc on a
/// transparent ground and anti-aliased edges, like a UI icon.
fn icon(w: u32, h: u32) -> Vec<u8> {
    let mut out = vec![0u8; (w * h * 4) as usize];
    let (cx, cy, r) = (w as f32 / 2.0, h as f32 / 2.0, w.min(h) as f32 * 0.4);
    for y in 0..h {
        for x in 0..w {
            let d = ((x as f32 + 0.5 - cx).powi(2) + (y as f32 + 0.5 - cy).powi(2)).sqrt();
            let a = (r - d + 0.5).clamp(0.0, 1.0);
            let at = ((y * w + x) * 4) as usize;
            out[at] = (40 + x * 180 / w) as u8;
            out[at + 1] = (200 - y * 150 / h) as u8;
            out[at + 2] = 90;
            out[at + 3] = (a * 255.0).round() as u8;
        }
    }
    out
}

/// Peak signal to noise ratio over the channels that count.
fn psnr(a: &[u8], b: &[u8], channels: &[usize]) -> f64 {
    let mut sum = 0f64;
    let mut n = 0f64;
    for (pa, pb) in a.chunks_exact(4).zip(b.chunks_exact(4)) {
        for &c in channels {
            sum += (f64::from(pa[c]) - f64::from(pb[c])).powi(2);
            n += 1.0;
        }
    }
    if sum == 0.0 { f64::INFINITY } else { 10.0 * (255.0f64.powi(2) / (sum / n)).log10() }
}

/// Colour error only where the texel is opaque enough to be seen.
fn psnr_visible(a: &[u8], b: &[u8]) -> f64 {
    let mut sum = 0f64;
    let mut n = 0f64;
    for (pa, pb) in a.chunks_exact(4).zip(b.chunks_exact(4)) {
        if pa[3] >= 200 {
            for c in 0..3 {
                sum += (f64::from(pa[c]) - f64::from(pb[c])).powi(2);
                n += 1.0;
            }
        }
    }
    if sum == 0.0 { f64::INFINITY } else { 10.0 * (255.0f64.powi(2) / (sum / n)).log10() }
}

#[test]
fn bc7_partition_tables_agree_with_their_anchors() {
    for p in 0..64 {
        let two = bc7::PARTITIONS_2[p].as_bytes();
        let three = bc7::PARTITIONS_3[p].as_bytes();
        assert_eq!(two.len(), 16);
        assert_eq!(three.len(), 16);
        assert_eq!(two[0], b'0', "partition {p}: texel 0 is in subset 0");
        assert_eq!(three[0], b'0', "partition {p}: texel 0 is in subset 0");
        assert_eq!(two[usize::from(bc7::ANCHOR_2[p])], b'1', "2-subset partition {p}");
        assert_eq!(three[usize::from(bc7::ANCHOR_3[0][p])], b'1', "3-subset partition {p}");
        assert_eq!(three[usize::from(bc7::ANCHOR_3[1][p])], b'2', "3-subset partition {p}");
        // The anchor is the first texel of its subset only in some
        // partitions, but no earlier texel of a subset may be an anchor of
        // another: each subset is present.
        assert!(two.contains(&b'1'));
        assert!(three.contains(&b'1') && three.contains(&b'2'));
    }
}

#[test]
fn bc7_constant_blocks_are_within_one() {
    for colour in [[0, 0, 0, 0], [255, 255, 255, 255], [12, 200, 77, 255], [128, 64, 32, 9], [1, 2, 3, 254]] {
        let block = bc7::encode6(&[colour; 16]);
        assert_eq!(block[0] & 0x7f, 0x40, "mode 6");
        let back = bc7::decode(&block);
        for p in back {
            for c in 0..4 {
                assert!((i32::from(p[c]) - i32::from(colour[c])).abs() <= 1, "{colour:?} -> {p:?}");
            }
        }
        // Channels of one parity are met exactly (one p-bit for all four).
        if colour.iter().all(|v| v % 2 == colour[0] % 2) {
            assert_eq!(back, [colour; 16]);
        }
    }
}

#[test]
fn bc7_decodes_a_hand_made_mode_5_block() {
    // Mode 5 (bit 5), rotation 0, R 127/127, G 0/0, B 64/64, A 200/200,
    // indices zero: every texel is (255, 0, 129, 200).
    let mut bits: u128 = 1 << 5;
    let mut at = 6;
    let mut put = |v: u128, n: u32| {
        bits |= v << at;
        at += n;
    };
    put(0, 2);
    for v in [127, 127, 0, 0, 64, 64] {
        put(v, 7);
    }
    put(200, 8);
    put(200, 8);
    assert_eq!(at, 6 + 2 + 42 + 16);
    let px = bc7::decode(&bits.to_le_bytes());
    // 7-bit 64 widens to (64 << 1) | (64 >> 6) = 129.
    assert_eq!(px, [[255, 0, 129, 200]; 16]);
}

#[test]
fn bc7_mode_5_keeps_alpha_apart_from_colour() {
    // A cut-out corner: clear texels, a light rim and a dark face, which no
    // one line through RGBA holds.
    let mut px = [[205u8, 205, 205, 255]; 16];
    for i in [0, 1, 4] {
        px[i] = [205, 205, 205, 0];
    }
    for i in [10, 11, 14, 15] {
        px[i] = [40, 40, 40, 255];
    }
    let five = bc7::encode5(&px);
    assert_eq!(five[0] & 0x3f, 0x20, "mode 5");
    let back = bc7::decode(&five);
    for (p, q) in px.iter().zip(&back) {
        assert_eq!(p[3], q[3], "alpha {p:?} -> {q:?}");
        if p[3] > 0 {
            for c in 0..3 {
                assert!((i32::from(p[c]) - i32::from(q[c])).abs() <= 2, "{p:?} -> {q:?}");
            }
        }
    }
    // And the chooser takes it over mode 6 here.
    assert_eq!(bc7::encode(&px), five);
}

#[test]
fn bc7_round_trip_of_an_icon() {
    let (w, h) = (64, 64);
    let img = icon(w, h);
    let enc = encode(Format::Bc7, w, h, &img).unwrap();
    assert_eq!(enc.len(), 16 * 16 * 16);
    let dec = decode(Format::Bc7, w, h, &enc, 16 * 16).unwrap();
    let rgb = psnr_visible(&img, &dec);
    let a = psnr(&img, &dec, &[3]);
    assert!(rgb > 38.0, "colour PSNR {rgb}");
    assert!(a > 38.0, "alpha PSNR {a}");
}

#[test]
fn bc7_random_blocks_stay_close() {
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    for _ in 0..200 {
        // Two random colours and texels between them, with a little noise.
        let a: [u32; 4] = std::array::from_fn(|_| rng.next() % 256);
        let b: [u32; 4] = std::array::from_fn(|_| rng.next() % 256);
        let px: [[u8; 4]; 16] = std::array::from_fn(|_| {
            let t = rng.next() % 17;
            std::array::from_fn(|c| ((a[c] * (16 - t) + b[c] * t) / 16 + rng.next() % 3).min(255) as u8)
        });
        let back = bc7::decode(&bc7::encode(&px));
        let worst =
            px.iter().zip(&back).flat_map(|(p, q)| (0..4).map(move |c| (i32::from(p[c]) - i32::from(q[c])).abs())).max();
        assert!(worst.unwrap() <= 16, "worst error {worst:?} for {px:?} -> {back:?}");
    }
}

#[test]
fn bc1_round_trip_keeps_colour_and_cut_out_alpha() {
    let (w, h) = (32, 32);
    let img = icon(w, h);
    let enc = encode(Format::Bc1, w, h, &img).unwrap();
    assert_eq!(enc.len(), 8 * 8 * 8);
    let dec = decode(Format::Bc1, w, h, &enc, 8 * 8).unwrap();
    assert!(psnr_visible(&img, &dec) > 30.0, "colour PSNR {}", psnr_visible(&img, &dec));
    for (p, q) in img.chunks_exact(4).zip(dec.chunks_exact(4)) {
        assert_eq!(q[3] == 255, p[3] >= 128, "alpha {} came back as {}", p[3], q[3]);
    }
}

#[test]
fn bc1_constant_colours_are_the_nearest_565() {
    for c in [[0u8, 0, 0], [255, 255, 255], [255, 0, 0], [0, 255, 0], [10, 20, 30]] {
        let block = encode_block(Format::Bc1, &[[c[0], c[1], c[2], 255]; 16]);
        let back = decode_block(Format::Bc1, &block);
        for p in back {
            for i in 0..3 {
                assert!((i32::from(p[i]) - i32::from(c[i])).abs() <= 4, "{c:?} -> {p:?}");
            }
            assert_eq!(p[3], 255);
        }
    }
}

#[test]
fn bc3_and_bc2_round_trip() {
    let (w, h) = (48, 40);
    let img = icon(w, h);
    for (format, alpha_db) in [(Format::Bc3, 35.0), (Format::Bc2, 28.0)] {
        let enc = encode(format, w, h, &img).unwrap();
        let (row, rows) = level_shape(format, w, h);
        assert_eq!(enc.len(), row * rows);
        let dec = decode(format, w, h, &enc, row).unwrap();
        assert!(psnr_visible(&img, &dec) > 30.0, "{format:?} colour PSNR {}", psnr_visible(&img, &dec));
        let a = psnr(&img, &dec, &[3]);
        assert!(a > alpha_db, "{format:?} alpha PSNR {a}");
    }
}

#[test]
fn bc4_round_trip_and_extremes() {
    let v: [u8; 16] = std::array::from_fn(|i| (i * 17) as u8);
    let px = v.map(|x| [x, x, x, 255]);
    let back = decode_block(Format::Bc4, &encode_block(Format::Bc4, &px));
    for (p, q) in px.iter().zip(back) {
        assert!((i32::from(p[0]) - i32::from(q[0])).abs() <= 19, "{} -> {}", p[0], q[0]);
    }
    // 0 and 255 come back exact.
    assert_eq!(back[0][0], 0);
    assert_eq!(back[15][0], 255);
    let flat = decode_block(Format::Bc4, &encode_block(Format::Bc4, &[[77, 77, 77, 255]; 16]));
    assert_eq!(flat, [[77, 77, 77, 255]; 16]);
}

#[test]
fn bc5_decodes_two_channels() {
    // Both halves flat: red 10, green 250.
    let mut block = [0u8; 16];
    block[0] = 10;
    block[1] = 10;
    block[8] = 250;
    block[9] = 250;
    assert_eq!(decode_block(Format::Bc5, &block), [[10, 250, 0, 255]; 16]);
    assert!(!Format::Bc5.encodable());
}

#[test]
fn uncompressed_formats_keep_their_channel_order() {
    let rgba = [1u8, 2, 3, 4, 5, 6, 7, 8];
    assert_eq!(decode(Format::Rgba8, 2, 1, &rgba, 8).unwrap(), rgba);
    assert_eq!(decode(Format::Bgra8, 2, 1, &rgba, 8).unwrap(), [3, 2, 1, 4, 7, 6, 5, 8]);
    assert_eq!(decode(Format::Bgrx8, 2, 1, &rgba, 8).unwrap(), [3, 2, 1, 255, 7, 6, 5, 255]);
    assert_eq!(decode(Format::R8, 2, 1, &[9, 10], 2).unwrap(), [9, 9, 9, 255, 10, 10, 10, 255]);
    for format in [Format::Rgba8, Format::Bgra8] {
        let enc = encode(format, 2, 1, &rgba).unwrap();
        assert_eq!(decode(format, 2, 1, &enc, 8).unwrap(), rgba);
    }
}

#[test]
fn encode_rect_leaves_other_elements_alone() {
    let (w, h) = (16, 16);
    for format in [Format::Bc1, Format::Bc3, Format::Bc7, Format::Rgba8, Format::R8] {
        let (row, rows) = level_shape(format, w, h);
        // A pitch wider than the row, as a staging buffer has.
        let pitch = row + 24;
        let mut bytes = vec![0xA5u8; pitch * rows];
        let white = vec![255u8; (w * h * 4) as usize];
        assert!(encode_rect(format, w, h, &white, &mut bytes, pitch, [4, 4, 8, 8]));
        let b = format.block() as usize;
        let n = format.element_bytes();
        for ey in 0..rows {
            for ex in 0..row / n {
                let inside = (ex * b) < 8 && (ex * b + b) > 4 && (ey * b) < 8 && (ey * b + b) > 4;
                let e = &bytes[ey * pitch + ex * n..ey * pitch + ex * n + n];
                assert_eq!(e.iter().all(|&v| v == 0xA5), !inside, "{format:?} element {ex},{ey}");
            }
            assert!(bytes[ey * pitch + row..(ey + 1) * pitch].iter().all(|&v| v == 0xA5), "padding kept");
        }
        let dec = decode(format, w, h, &bytes, pitch).unwrap();
        let at = ((5 * w + 5) * 4) as usize;
        assert!(dec[at] >= 250, "{format:?} painted texel is {:?}", &dec[at..at + 4]);
    }
}

#[test]
fn odd_sizes_and_partial_blocks() {
    let (w, h) = (13, 7);
    let img = icon(w, h);
    for format in [Format::Bc1, Format::Bc3, Format::Bc7] {
        let enc = encode(format, w, h, &img).unwrap();
        let (row, _) = level_shape(format, w, h);
        let dec = decode(format, w, h, &enc, row).unwrap();
        assert_eq!(dec.len(), (w * h * 4) as usize);
        // Mode 6 has one colour line a block: this image's colours change
        // fast along two axes besides its alpha, which costs BC7 here.
        let floor = if format == Format::Bc7 { 24.0 } else { 28.0 };
        assert!(psnr_visible(&img, &dec) > floor, "{format:?} {}", psnr_visible(&img, &dec));
    }
    assert!(decode(Format::Bc7, 16, 16, &[0u8; 10], 64).is_none());
}

#[test]
fn dxgi_formats_map() {
    assert_eq!(Format::from_dxgi(28), Some(Format::Rgba8));
    assert_eq!(Format::from_dxgi(91), Some(Format::Bgra8));
    assert_eq!(Format::from_dxgi(71), Some(Format::Bc1));
    assert_eq!(Format::from_dxgi(77), Some(Format::Bc3));
    assert_eq!(Format::from_dxgi(98), Some(Format::Bc7));
    assert_eq!(Format::from_dxgi(2), None);
}
